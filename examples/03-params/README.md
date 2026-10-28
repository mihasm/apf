# 03 – Parameters

A small channel tool that uses every kind of parameter.

**What it teaches**

- the kinds: `float`, `int`, `bool`, `choice`, and `bypass`
- curves: `Linear` (the default), `Log` for frequencies, `Power` for the quiet end of a range
- units, decimals, `module` (groups), `not_automatable`
- the three rules of parameters: **never rename a key, only append, never change what a
  stored value means** – see the comment at the top of `src/lib.rs`
- why: the id a host stores is a hash of the key, and Audio Units address parameters by position

**Build and try it**

```sh
cargo test -p example-03-params
cargo xtask bundle example-03-params
```

**Look at**

- `src/lib.rs` – the `PARAMS` list.
- `tests/process.rs` – `PINNED_IDS` fails the day somebody renames a key or reorders the list;
  copy that test into your own plugin once it ships.

Previous: [02 – Gain](../02-gain) · Next: [04 – Envelope](../04-envelope)
