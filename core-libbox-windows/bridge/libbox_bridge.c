/*
 * libbox_bridge.c — C shim between gobind's `libbox` Windows export and
 * the Rust `extern "C"` declarations in src/bridge.rs.
 *
 * Why a shim and not direct FFI to libbox.h:
 *
 *   gobind on Windows emits headers that use `GoString`, `GoSlice`, and
 *   pointer-to-Go-managed-handle types. The Rust side would need to know
 *   about all of those types to call gobind's functions directly, which
 *   pulls a lot of `<stdint.h>`-style baggage into bridge.rs and forces
 *   the Rust side to track gobind ABI changes whenever sing-box bumps
 *   its toolchain. By doing the conversion HERE, in C, the Rust side
 *   only sees plain `*const c_char` / `*mut c_void` / `int` arguments —
 *   the same surface as the macOS Obj-C bridge — and `core.rs` can be
 *   shared between platforms with at most a `cfg(target_os)` switch on
 *   the bridge module path.
 *
 * What this file is right now:
 *
 *   A skeleton with stub implementations that match the gobind C
 *   surface but route to NULL / error sentinels. Once a real
 *   `libbox.h` lands at frameworks/libbox/libbox.h, replace the stubs
 *   with calls to the real exports:
 *
 *     LibboxNewService(GoString config, char **err) -> uintptr_t
 *     LibboxBoxService_Start(uintptr_t self, char **err) -> int
 *     LibboxBoxService_Close(uintptr_t self, char **err) -> int
 *     LibboxBoxService_Release(uintptr_t self) -> void
 *     LibboxVersion() -> char *
 *
 *   The actual symbol names depend on what gobind emits — fill them in
 *   when the artifacts arrive. The Rust side uses the wrapper names
 *   below (pingle_libbox_*) regardless, so substituting the real call
 *   targets is a one-line change per function.
 *
 * Memory ownership:
 *
 *   - `cfg_json` is borrowed for the duration of the call. The bridge
 *     copies it into Go's heap (gobind handles this internally) before
 *     returning.
 *   - `*err` is allocated by gobind (or by us, in the stub) and the
 *     Rust side is responsible for freeing it via pingle_libbox_free_string.
 *   - The opaque service handle is owned by Go's runtime; the Rust
 *     side calls pingle_libbox_service_release exactly once when done.
 */

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* Keep the function signatures in sync with src/bridge.rs. */

/*
 * Wrapper for LibboxNewService.
 *
 * `cfg_json` is a NUL-terminated UTF-8 JSON string (the contents of the
 * sing-box config file). On success returns a non-NULL opaque handle.
 * On failure returns NULL and writes a heap-allocated error string into
 * *err — caller frees with pingle_libbox_free_string.
 */
void *pingle_libbox_new_service(const char *cfg_json, char **err) {
    (void)cfg_json;
    if (err) {
        const char *msg = "libbox stub: real libbox.dll not yet integrated";
        size_t len = strlen(msg);
        char *copy = (char *)malloc(len + 1);
        if (copy) {
            memcpy(copy, msg, len + 1);
            *err = copy;
        }
    }
    return NULL;
}

/*
 * Wrapper for LibboxBoxService.start. Returns 1 on success, 0 on
 * failure (with *err set). The handle remains valid on failure but
 * the caller should not invoke close on it.
 */
int pingle_libbox_service_start(void *handle, char **err) {
    (void)handle;
    if (err) {
        const char *msg = "libbox stub: start unimplemented";
        size_t len = strlen(msg);
        char *copy = (char *)malloc(len + 1);
        if (copy) {
            memcpy(copy, msg, len + 1);
            *err = copy;
        }
    }
    return 0;
}

/*
 * Wrapper for LibboxBoxService.close. Same return contract as start.
 */
int pingle_libbox_service_close(void *handle, char **err) {
    (void)handle;
    if (err) {
        const char *msg = "libbox stub: close unimplemented";
        size_t len = strlen(msg);
        char *copy = (char *)malloc(len + 1);
        if (copy) {
            memcpy(copy, msg, len + 1);
            *err = copy;
        }
    }
    return 0;
}

/*
 * Release the opaque handle. Always safe — passing NULL is a no-op.
 */
void pingle_libbox_service_release(void *handle) {
    (void)handle;
}

/*
 * Returns a heap-allocated NUL-terminated version string. Caller frees
 * with pingle_libbox_free_string. Returns NULL on failure.
 */
char *pingle_libbox_version(void) {
    const char *msg = "libbox-stub-windows";
    size_t len = strlen(msg);
    char *copy = (char *)malloc(len + 1);
    if (copy) memcpy(copy, msg, len + 1);
    return copy;
}

/*
 * Free a string previously returned by any of the bridge functions.
 * Centralised here so the Rust side never has to know which allocator
 * (Go's heap, libc malloc, etc.) produced the buffer.
 */
void pingle_libbox_free_string(char *p) {
    if (p) free(p);
}
