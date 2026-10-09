;; minimal.wat — smallest real module that passes hilo's wasm header check.
;;
;; Build (one line, requires wabt >= 1.0 — the `wat2wasm` binary):
;;   wat2wasm minimal.wat -o minimal.wasm
;;
;; The compiled module is committed as `minimal.wasm` so the example works
;; with no toolchain installed. Regenerate it with the command above after
;; editing this file.
(module
  ;; A single exported function so the module is a genuine, runnable wasm
  ;; module rather than a bare 8-byte header. hilo does not execute plugins
  ;; yet (DF-WARPFS-59), so the body is an inert constant.
  (func (export "run") (result i32)
    i32.const 0)
)
