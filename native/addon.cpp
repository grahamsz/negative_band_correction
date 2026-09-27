// SPDX-License-Identifier: MIT OR Apache-2.0
// Small UXP adapter; all detection, correction and job ownership live in Rust.
#include "UxpAddon.h"
#include "banding.h"
#include <cstring>
#include <stdexcept>
#include <vector>

namespace {
void check(addon_status status) {
    if (status != addon_ok) throw std::runtime_error("UXP native buffer operation failed");
}
struct Reply {
    BandingBuffer value;
    ~Reply() {banding_buffer_free(value);}
};
addon_value dispatch(addon_env env, addon_callback_info info) noexcept {
    try {
        addon_value args[2] = {nullptr, nullptr};
        size_t argc = 2;
        check(UxpAddonApis.uxp_addon_get_cb_info(env, info, &argc, args, nullptr, nullptr));
        if (argc != 2) throw std::runtime_error("Expected request and pixel buffer");
        size_t length = 0;
        check(UxpAddonApis.uxp_addon_get_value_string_utf8(env, args[0], nullptr, 0, &length));
        if (length > 65536) throw std::runtime_error("Request too large");
        std::vector<char> request(length + 1);
        check(UxpAddonApis.uxp_addon_get_value_string_utf8(env, args[0], request.data(), request.size(), &length));
        void* pixels = nullptr;
        size_t size = 0;
        check(UxpAddonApis.uxp_addon_get_arraybuffer_info(env, args[1], &pixels, &size));
        // Rust borrows this buffer only for this call. The long-running fit runs
        // on Rust's worker; no Photoshop/UXP APIs are called from that worker.
        Reply reply{banding_dispatch(reinterpret_cast<const uint8_t*>(request.data()), length,
                                     static_cast<const uint8_t*>(pixels), size)};
        if (reply.value.kind == 2) {
            throw std::runtime_error(std::string(reinterpret_cast<char*>(reply.value.data), reply.value.len));
        }
        addon_value result = nullptr;
        if (reply.value.kind == 1) {
            void* destination = nullptr;
            check(UxpAddonApis.uxp_addon_create_arraybuffer(env, reply.value.len, &destination, &result));
            if (reply.value.len) std::memcpy(destination, reply.value.data, reply.value.len);
        } else {
            check(UxpAddonApis.uxp_addon_create_string_utf8(env, reinterpret_cast<char*>(reply.value.data), reply.value.len, &result));
        }
        return result;
    } catch (const std::exception& e) {
        UxpAddonApis.uxp_addon_throw_error(env, nullptr, e.what());
    } catch (...) {
        UxpAddonApis.uxp_addon_throw_error(env, nullptr, "Unexpected native bridge error");
    }
    return nullptr;
}
addon_value init(addon_env env, addon_value exports, const addon_apis&) {
    addon_value fn = nullptr;
    check(UxpAddonApis.uxp_addon_create_function(env, "dispatch", 8, dispatch, nullptr, &fn));
    check(UxpAddonApis.uxp_addon_set_named_property(env, exports, "dispatch", fn));
    return exports;
}
void terminate(addon_env) {banding_shutdown();}
}
UXP_ADDON_INIT(init)
UXP_ADDON_TERMINATE(terminate)
