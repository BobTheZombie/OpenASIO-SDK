# OpenASIO SDK (draft v0.1.0)

This tarball contains the **OpenASIO** C header and Markdown spec.

## Layout
```
include/openasio/openasio.h   # C99 ABI header (MIT OR Apache-2.0)
docs/openasio-spec.md         # Human-readable spec (MIT OR Apache-2.0)
LICENSE-MIT
LICENSE-APACHE
```

## Usage
- Vendors: implement the functions in `oa_driver_vtable` and export the two factory symbols.
- Hosts: `dlopen` the driver, create with `openasio_driver_create`, and start with `start(&cfg)`.

## Status
Early draft; API surface is intentionally small to stay stable.
