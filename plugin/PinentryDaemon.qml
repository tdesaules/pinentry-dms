import QtQuick
import Quickshell
import Quickshell.Io
import qs.Common
import qs.Services
import qs.Widgets
import qs.Modules.Plugins

// Daemon plugin: receives IPC `prompt` calls from the `pinentry-dms` Rust
// binary, shows a native DMS FloatingWindow, and writes the user's answer back
// over the Unix socket the binary is listening on.
//
// The modal is declared inline as a `property Component` (not loaded via
// Qt.createComponent on a sibling file) to avoid the "File name case mismatch"
// warning on hosts where /home is a symlink to /var/home. Same convention as
// the gopass-dms plugin.
PluginComponent {
    id: root

    property var activeModal: null
    property string activeSocketPath: ""
    property var pendingQueue: []
    // Sockets awaiting the SAME getpin response as the active modal. When N
    // gopass processes prompt concurrently for the same passphrase, we show
    // ONE modal and broadcast the result to the active socket + all these.
    property var pendingGetpinSockets: []

    IpcHandler {
        target: "pinentryDms"

        function prompt(requestJson: string): string {
            try {
                const req = JSON.parse(requestJson);
                root.showModal(req);
                return "OK";
            } catch (e) {
                console.error("PinentryDms: failed to parse request:", e);
                return "ERR";
            }
        }
    }

    function showModal(req) {
        const kind = req.type || "getpin"

        // Dedup concurrent getpin prompts: if a getpin modal is already
        // active, attach this caller's socket to it. When the active modal
        // resolves, the passphrase is broadcast to all attached sockets, so
        // the user types once and every concurrent gopass process is served.
        // This avoids a storm of modals when many callers hit a locked agent
        // at once (e.g. boot-time git fetch + chezmoi apply + IDE git fetch).
        if (activeModal && kind === "getpin") {
            pendingGetpinSockets.push(req.socket || "")
            return
        }

        // Non-getpin prompts, or the first getpin: if a different modal is
        // active, queue it for sequential display (no dedup for
        // confirm/message — they are distinct operations).
        if (activeModal) {
            pendingQueue.push(req)
            return
        }

        activeSocketPath = req.socket || ""

        activeModal = pinentryModalComponent.createObject(root, {
            "modalType": kind,
            "title": req.title || "Pinentry",
            "desc": req.desc || "",
            "prompt": req.prompt || "",
            "errorText": req.error || "",
            "keyInfo": req.keyInfo || "",
            "repeatError": req.repeatError || "",
            "okLabel": req.okLabel || "",
            "cancelLabel": req.cancelLabel || "",
            "notOkLabel": req.notOkLabel || "",
            "timeout": req.timeout || 0,
            "repeat": req.repeat || false
        });

        activeModal.submitted.connect(root.handleSubmit);
        activeModal.confirmed.connect(root.handleConfirmOK);
        activeModal.cancelled.connect(root.handleCancel);
        activeModal.rejectedNotOK.connect(root.handleNotOK);
        activeModal.timedOut.connect(root.handleTimeout);
        activeModal.show();
    }

    // Send `response` to the active socket AND any sockets waiting on the
    // same getpin (dedup broadcast), then close the modal. `response` is a
    // JS object like {"type":"pin","value":"..."}.
    function sendResponse(response) {
        const json = JSON.stringify(response)

        // Active socket (may be empty if already consumed — defensive).
        if (activeSocketPath) {
            root._writeSocket(activeSocketPath, json)
            activeSocketPath = ""
        }
        // Broadcast to dedup'd getpin waiters.
        for (let i = 0; i < pendingGetpinSockets.length; i++) {
            root._writeSocket(pendingGetpinSockets[i], json)
        }
        pendingGetpinSockets = []
    }

    // Open a socket to `path`, write `payload + "\n"`, self-destruct. Used
    // for both the single active response and the dedup broadcast.
    function _writeSocket(path, payload) {
        if (!path) {
            console.error("PinentryDms: empty socket path, dropping response")
            return
        }
        const sock = responseSocketComponent.createObject(root, {
            "path": path,
            "payload": payload
        })
        sock.connected = true
    }

    function handleSubmit(value) {
        root.sendResponse({"type": "pin", "value": value});
        root.closeModal();
    }

    function handleConfirmOK() {
        root.sendResponse({"type": "ok"});
        root.closeModal();
    }

    function handleCancel() {
        root.sendResponse({"type": "cancel"});
        root.closeModal();
    }

    function handleNotOK() {
        root.sendResponse({"type": "notok"});
        root.closeModal();
    }

    function handleTimeout() {
        root.sendResponse({"type": "timeout"});
        root.closeModal();
    }

    function closeModal() {
        if (activeModal) {
            activeModal.close();
            activeModal.destroy();
            activeModal = null;
        }
        // Drain the queue: show the next pending prompt, if any.
        if (pendingQueue.length > 0) {
            const next = pendingQueue.shift();
            showModal(next);
        }
    }

    // Response socket: connects to the path the binary is listening on and
    // writes the JSON response + newline, then self-destructs.
    Component {
        id: responseSocketComponent

        Socket {
            property string payload: ""

            onConnectionStateChanged: {
                if (connected) {
                    write(payload + "\n");
                    flush();
                    connected = false;
                    Qt.callLater(destroy);
                }
            }
        }
    }

    property Component pinentryModalComponent: Component {
        FloatingWindow {
            id: modal

            property string modalType: "getpin"
            property string desc: ""
            property string prompt: ""
            property string errorText: ""
            property string keyInfo: ""
            property string repeatError: ""
            property string okLabel: ""
            property string cancelLabel: ""
            property string notOkLabel: ""
            property int timeout: 0
            property bool repeat: false

            signal submitted(string value)
            signal confirmed()
            signal cancelled()
            signal rejectedNotOK()
            signal timedOut()

            property bool disablePopupTransparency: true
            property string passwordInput: ""
            property string repeatInput: ""
            property bool showRepeatField: false
            property bool repeatMismatch: false
            readonly property int inputFieldHeight: Theme.fontSizeMedium + Theme.spacingL * 2
            readonly property bool isGetPin: modalType === "getpin"
            readonly property bool isConfirm: modalType === "confirm"
            readonly property bool isMessage: modalType === "message"
            readonly property string resolvedOkLabel: okLabel || "OK"
            readonly property string resolvedCancelLabel: cancelLabel || "Cancel"

            objectName: "pinentryDmsModal"
            title: "Pinentry"
            minimumSize: Qt.size(460, Math.ceil(mainColumn.implicitHeight + Theme.spacingM * 2))
            maximumSize: minimumSize
            color: Theme.surfaceContainer
            visible: false

            function show() {
                passwordInput = "";
                repeatInput = "";
                showRepeatField = false;
                repeatMismatch = false;
                visible = true;
                Qt.callLater(focusInput);
            }

            function close() {
                visible = false;
            }

            function focusInput() {
                if (isGetPin)
                    passwordField.forceActiveFocus();
                else
                    okButton.forceActiveFocus();
            }

            function submit() {
                if (isGetPin) {
                    if (repeat && !showRepeatField) {
                        showRepeatField = true;
                        Qt.callLater(() => repeatField.forceActiveFocus());
                        return;
                    }
                    if (repeat && passwordInput !== repeatInput) {
                        repeatMismatch = true;
                        Qt.callLater(() => repeatField.forceActiveFocus());
                        return;
                    }
                    const value = passwordInput;
                    // Zeroize the in-memory copies as soon as the value has been
                    // handed off to the response handler, so the passphrase
                    // does not linger in QML string bindings.
                    passwordInput = "";
                    repeatInput = "";
                    submitted(value);
                } else {
                    confirmed();
                }
            }

            function cancel() {
                cancelled();
            }

            function focusableButtons() {
                const list = [];
                if (isConfirm && notOkLabel !== "")
                    list.push(notOkButton);
                if (!isMessage)
                    list.push(cancelButton);
                list.push(okButton);
                return list;
            }

            function handleNav(event, current, vertOnly) {
                const k = event.key;
                const ctrl = (event.modifiers & Qt.ControlModifier) !== 0;
                const left  = !vertOnly && (k === Qt.Key_Left  || (ctrl && k === Qt.Key_H));
                const right = !vertOnly && (k === Qt.Key_Right || (ctrl && k === Qt.Key_L));
                const up    = k === Qt.Key_Up   || (ctrl && k === Qt.Key_K);
                const down  = k === Qt.Key_Down || (ctrl && k === Qt.Key_J);
                if (!(left || right || up || down))
                    return false;

                const btns = focusableButtons();
                const idx = btns.indexOf(current);

                if (left && idx > 0) {
                    btns[idx - 1].forceActiveFocus();
                    return true;
                }
                if (right && idx >= 0 && idx < btns.length - 1) {
                    btns[idx + 1].forceActiveFocus();
                    return true;
                }
                if (up) {
                    if (current === repeatField) {
                        passwordField.forceActiveFocus();
                        return true;
                    }
                    if (isGetPin && idx >= 0) {
                        (showRepeatField ? repeatField : passwordField).forceActiveFocus();
                        return true;
                    }
                }
                if (down) {
                    if (current === passwordField && showRepeatField) {
                        repeatField.forceActiveFocus();
                        return true;
                    }
                    if (current === passwordField || current === repeatField) {
                        okButton.forceActiveFocus();
                        return true;
                    }
                }
                return false;
            }

            onVisibleChanged: {
                if (visible) {
                    Qt.callLater(focusInput);
                    if (timeout > 0)
                        timeoutTimer.start();
                    return;
                }
                passwordInput = "";
                repeatInput = "";
                timeoutTimer.stop();
            }

            Timer {
                id: timeoutTimer
                interval: modal.timeout > 0 ? modal.timeout * 1000 : 60000
                repeat: false
                onTriggered: modal.timedOut()
            }

            FloatingWindowControls {
                id: windowControls
                targetWindow: modal
            }

            FocusScope {
                id: contentFocusScope
                anchors.fill: parent
                focus: true

                Keys.onEscapePressed: event => {
                    cancel();
                    event.accepted = true;
                }

                Column {
                    id: mainColumn
                    anchors.fill: parent
                    anchors.margins: Theme.spacingM
                    spacing: Theme.spacingS

                    // Header row with title and close button
                    Item {
                        width: parent.width
                        height: Math.max(titleColumn.implicitHeight, closeBtn.implicitHeight)

                        MouseArea {
                            anchors.fill: parent
                            onPressed: windowControls.tryStartMove()
                        }

                        Column {
                            id: titleColumn
                            anchors.left: parent.left
                            anchors.right: closeBtn.left
                            anchors.rightMargin: Theme.spacingM
                            spacing: Theme.spacingXS

                            StyledText {
                                text: modal.title || "Pinentry"
                                font.pixelSize: Theme.fontSizeLarge
                                color: Theme.surfaceText
                                font.weight: Font.Medium
                            }

                            StyledText {
                                text: modal.desc
                                font.pixelSize: Theme.fontSizeMedium
                                color: Theme.surfaceTextMedium
                                width: parent.width
                                wrapMode: Text.Wrap
                                maximumLineCount: 3
                                elide: Text.ElideRight
                                visible: text !== ""
                            }
                        }

                        DankActionButton {
                            id: closeBtn
                            anchors.right: parent.right
                            anchors.top: parent.top
                            iconName: "-close"
                            iconSize: Theme.iconSize - 4
                            iconColor: Theme.surfaceText
                            onClicked: cancel()
                        }
                    }

                    // Error / mismatch banner — kept near the top so users see
                    // it before re-typing. Uses the SETREPEAT error text when
                    // provided, falling back to a generic message.
                    StyledText {
                        text: repeatMismatch ? (modal.repeatError || "Passphrases do not match") : modal.errorText
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.error
                        width: parent.width
                        wrapMode: Text.Wrap
                        visible: text !== ""
                        height: visible ? implicitHeight : 0
                    }

                    // Prompt label
                    StyledText {
                        text: modal.prompt
                        font.pixelSize: Theme.fontSizeMedium
                        color: Theme.surfaceText
                        width: parent.width
                        visible: text !== "" && isGetPin
                        height: visible ? implicitHeight : 0
                    }

                    // Key info — which key/secret is being unlocked (SETKEYINFO).
                    // Discrete monospace label under the prompt.
                    StyledText {
                        text: modal.keyInfo
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceTextMedium
                        width: parent.width
                        wrapMode: Text.Wrap
                        visible: text !== "" && isGetPin
                        height: visible ? implicitHeight : 0
                    }

                    // Password input
                    Rectangle {
                        width: parent.width
                        height: visible ? inputFieldHeight : 0
                        radius: Theme.cornerRadius
                        color: Theme.surfaceHover
                        border.color: passwordField.activeFocus ? Theme.primary : Theme.outlineStrong
                        border.width: passwordField.activeFocus ? 2 : 1
                        visible: isGetPin

                        MouseArea {
                            anchors.fill: parent
                            onClicked: passwordField.forceActiveFocus()
                        }

                        DankTextField {
                            id: passwordField
                            anchors.fill: parent
                            font.pixelSize: Theme.fontSizeMedium
                            textColor: Theme.surfaceText
                            text: passwordInput
                            showPasswordToggle: true
                            echoMode: passwordVisible ? TextInput.Normal : TextInput.Password
                            placeholderText: ""
                            backgroundColor: "transparent"
                            ignoreUpDownKeys: true
                            onTextEdited: passwordInput = text
                            onAccepted: submit()
                            Keys.onPressed: event => {
                                if (handleNav(event, passwordField, true))
                                    event.accepted = true;
                            }
                        }
                    }

                    // Repeat password input
                    Rectangle {
                        width: parent.width
                        height: visible ? inputFieldHeight : 0
                        radius: Theme.cornerRadius
                        color: Theme.surfaceHover
                        border.color: repeatField.activeFocus ? Theme.primary : Theme.outlineStrong
                        border.width: repeatField.activeFocus ? 2 : 1
                        visible: isGetPin && showRepeatField

                        MouseArea {
                            anchors.fill: parent
                            onClicked: repeatField.forceActiveFocus()
                        }

                        DankTextField {
                            id: repeatField
                            anchors.fill: parent
                            font.pixelSize: Theme.fontSizeMedium
                            textColor: Theme.surfaceText
                            text: repeatInput
                            showPasswordToggle: true
                            echoMode: passwordVisible ? TextInput.Normal : TextInput.Password
                            placeholderText: "Repeat passphrase"
                            backgroundColor: "transparent"
                            ignoreUpDownKeys: true
                            onTextEdited: {
                                repeatInput = text;
                                repeatMismatch = false;
                            }
                            onAccepted: submit()
                            Keys.onPressed: event => {
                                if (handleNav(event, repeatField, true))
                                    event.accepted = true;
                            }
                        }
                    }

                    // Button row
                    Item {
                        width: parent.width
                        height: 36

                        Row {
                            anchors.right: parent.right
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: Theme.spacingM

                            // Not OK button (3-button confirm)
                            Rectangle {
                                id: notOkButton
                                width: Math.max(70, notOkText.contentWidth + Theme.spacingM * 2)
                                height: 36
                                radius: Theme.cornerRadius
                                color: notOkArea.containsMouse ? Theme.surfaceTextHover : "transparent"
                                border.color: activeFocus ? Theme.surfaceText : Theme.surfaceVariantAlpha
                                border.width: activeFocus ? 2 : 1
                                visible: isConfirm && modal.notOkLabel !== ""
                                activeFocusOnTab: true
                                Keys.onReturnPressed: modal.rejectedNotOK()
                                Keys.onEnterPressed: modal.rejectedNotOK()
                                Keys.onSpacePressed: modal.rejectedNotOK()
                                Keys.onPressed: event => {
                                    if (handleNav(event, notOkButton, false))
                                        event.accepted = true;
                                }

                                StyledText {
                                    id: notOkText
                                    anchors.centerIn: parent
                                    text: modal.notOkLabel
                                    font.pixelSize: Theme.fontSizeMedium
                                    color: Theme.surfaceText
                                    font.weight: Font.Medium
                                }

                                MouseArea {
                                    id: notOkArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: modal.rejectedNotOK()
                                }
                            }

                            // Cancel button
                            Rectangle {
                                id: cancelButton
                                width: Math.max(70, cancelText.contentWidth + Theme.spacingM * 2)
                                height: 36
                                radius: Theme.cornerRadius
                                color: cancelArea.containsMouse ? Theme.surfaceTextHover : "transparent"
                                border.color: activeFocus ? Theme.surfaceText : Theme.surfaceVariantAlpha
                                border.width: activeFocus ? 2 : 1
                                visible: !isMessage
                                activeFocusOnTab: true
                                Keys.onReturnPressed: cancel()
                                Keys.onEnterPressed: cancel()
                                Keys.onSpacePressed: cancel()
                                Keys.onPressed: event => {
                                    if (handleNav(event, cancelButton, false))
                                        event.accepted = true;
                                }

                                StyledText {
                                    id: cancelText
                                    anchors.centerIn: parent
                                    text: resolvedCancelLabel
                                    font.pixelSize: Theme.fontSizeMedium
                                    color: Theme.surfaceText
                                    font.weight: Font.Medium
                                }

                                MouseArea {
                                    id: cancelArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: cancel()
                                }
                            }

                            // OK / Submit button
                            Rectangle {
                                id: okButton
                                width: Math.max(80, okText.contentWidth + Theme.spacingM * 2)
                                height: 36
                                radius: Theme.cornerRadius
                                color: okArea.containsMouse ? Qt.darker(Theme.primary, 1.1) : Theme.primary
                                activeFocusOnTab: true
                                border.color: activeFocus ? Theme.surfaceText : "transparent"
                                border.width: activeFocus ? 2 : 0
                                Keys.onReturnPressed: submit()
                                Keys.onEnterPressed: submit()
                                Keys.onSpacePressed: submit()
                                Keys.onPressed: event => {
                                    if (handleNav(event, okButton, false))
                                        event.accepted = true;
                                }

                                StyledText {
                                    id: okText
                                    anchors.centerIn: parent
                                    text: resolvedOkLabel
                                    font.pixelSize: Theme.fontSizeMedium
                                    color: Theme.background
                                    font.weight: Font.Medium
                                }

                                MouseArea {
                                    id: okArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: submit()
                                }

                                Behavior on color {
                                    ColorAnimation {
                                        duration: Theme.shortDuration
                                        easing.type: Theme.standardEasing
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Component.onCompleted: {
        console.info("PinentryDms: daemon started");
    }

    Component.onDestruction: {
        if (activeModal)
            activeModal.destroy();
        console.info("PinentryDms: daemon stopped");
    }
}