// SPDX-License-Identifier: MIT OR Apache-2.0
#pragma once
#include <cstddef>
#include <cstdint>
extern "C" {
struct BandingBuffer { uint8_t* data; size_t len; uint32_t kind; };
BandingBuffer banding_dispatch(const uint8_t* request, size_t request_len,
                              const uint8_t* pixels, size_t pixels_len);
void banding_buffer_free(BandingBuffer buffer);
void banding_shutdown();
}
