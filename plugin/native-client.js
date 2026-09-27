"use strict";
// Same small request interface as the prototype, but no HTTP or pairing.
function createNativeClient(addon) {
    if (!addon || typeof addon.dispatch !== "function") throw new Error("Native banding module did not load.");
    return async (path, method = "GET", body, binary = false) => {
        const pixels = body instanceof ArrayBuffer ? body : new ArrayBuffer(0);
        const message = JSON.stringify({path, method, body:body instanceof ArrayBuffer ? null : body});
        const result = addon.dispatch(message, pixels);
        if (binary) {
            if (!(result instanceof ArrayBuffer)) throw new Error("Native engine returned an invalid pixel buffer.");
            return result;
        }
        if (typeof result !== "string") throw new Error("Native engine returned an invalid reply.");
        return JSON.parse(result);
    };
}
module.exports = {createNativeClient};
