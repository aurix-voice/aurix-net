const AurixNet = {
    next: 1,
    sockets: {},
    timers: {},
};

function AurixNetOpen(urlPtr, onOpen, onMessage, onClose) {
    const id = AurixNet.next++;
    const socket = new WebSocket(UTF8ToString(urlPtr));
    socket.binaryType = "arraybuffer";
    AurixNet.sockets[id] = socket;
    socket.onopen = function () {
        {{{ makeDynCall('vi', 'onOpen') }}}(id);
    };
    socket.onmessage = function (event) {
        const text = typeof event.data === "string";
        const length = text ? lengthBytesUTF8(event.data) : event.data.byteLength;
        const buffer = _malloc(length + 1) >>> 0;
        if (text) {
            stringToUTF8(event.data, buffer, length + 1);
        } else {
            HEAPU8.set(new Uint8Array(event.data), buffer);
        }
        try {
            {{{ makeDynCall('viiii', 'onMessage') }}}(id, buffer, length, text ? 1 : 0);
        } finally {
            _free(buffer);
        }
    };
    socket.onclose = function (event) {
        delete AurixNet.sockets[id];
        {{{ makeDynCall('vii', 'onClose') }}}(id, event.code);
    };
    return id;
}

function AurixNetSend(id, dataPtr, length, text) {
    const socket = AurixNet.sockets[id];
    if (socket && socket.readyState === WebSocket.OPEN) {
        const start = dataPtr >>> 0;
        socket.send(text ? UTF8ToString(start, length) : HEAPU8.slice(start, start + length));
    }
}

function AurixNetClose(id) {
    const socket = AurixNet.sockets[id];
    if (socket) {
        delete AurixNet.sockets[id];
        socket.onopen = socket.onmessage = socket.onclose = null;
        socket.close();
    }
}

function AurixNetEvery(ms, onTimer) {
    const id = AurixNet.next++;
    const timer = setInterval(function () {
        {{{ makeDynCall('vi', 'onTimer') }}}(id);
    }, ms);
    AurixNet.timers[id] = function () {
        clearInterval(timer);
    };
    return id;
}

function AurixNetAfter(ms, onTimer) {
    const id = AurixNet.next++;
    const timer = setTimeout(function () {
        delete AurixNet.timers[id];
        {{{ makeDynCall('vi', 'onTimer') }}}(id);
    }, ms);
    AurixNet.timers[id] = function () {
        clearTimeout(timer);
    };
    return id;
}

function AurixNetCancel(id) {
    const cancel = AurixNet.timers[id];
    if (cancel) {
        delete AurixNet.timers[id];
        cancel();
    }
}

const AurixNetLibrary = {
    $AurixNet: AurixNet,
    AurixNetOpen,
    AurixNetSend,
    AurixNetClose,
    AurixNetEvery,
    AurixNetAfter,
    AurixNetCancel,
};

autoAddDeps(AurixNetLibrary, "$AurixNet");
mergeInto(LibraryManager.library, AurixNetLibrary);
