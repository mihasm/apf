# 03 Params: every kind of parameter

A small channel tool, built to show every kind of parameter.

**You will learn**

- `float`, `int`, `bool`, `choice` and `bypass`
- curves (`Linear`, `Log`, `Power`), units, decimals, `module`, `not_automatable`
- the three rules: never rename a key, only append, never change a value's meaning
- how a test can pin the ids so nobody breaks them by accident

The code is in `examples/03-params/`. It is included here as it is on disk.

```rust,no_run
# extern crate apf;
{{#include ../../../examples/03-params/src/lib.rs}}
```

## Reading it

Read the comment at the top of the file, then the `PARAMS` list: each line is a different kind or
option. The rest of the file is ordinary DSP that gives every parameter something to do.

**The ids test.** `PINNED_IDS` in `tests/process.rs` lists the 32-bit id of every parameter, in
order. If someone renames a key, reorders the list or removes a parameter, the test fails with the
numbers side by side. Once your plugin has shipped, that failure is a bug report from the
future: saved projects would break.

**Display text.** The test also checks what a host shows: "2.00 kHz" for the middle of the log
range, "25 %" for half of a squared curve, "Mono" for a choice. All of it comes from the
definitions.

## Try it

- Append a `bool` parameter after `bypass`, and watch `PINNED_IDS` still pass for the old eight.
- Insert one in the middle and watch it fail... then undo.
- Change the curve of `drive` to `Linear` and see the text test ("25 %" at half travel) fail.

## Build and test

```sh
cargo test -p example-03-params
cargo xtask bundle example-03-params
```
