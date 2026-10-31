# Third-party notices

RaSTA-Rust is original Rust code. Its protocol behaviour was derived by
observing three public, MIT-licensed RaSTA implementations at the revisions
pinned in [docs/provenance.md](docs/provenance.md). No source code has been
copied from them. Some tests use values taken from, or computed with, those
implementations:

- `tests/safety_code.rs`: the 28-byte example message, its MD4 digests with the
  standard and a custom initial value, and that custom initial value come from
  SBB's `unit_test_srmd4.cc`, which cites the message as the example in
  Annex A of DIN VDE V 0831-200. The padding-boundary digests were computed
  with EULYNX Live's `rastamd4`.
- `tests/redundancy.rs`: the CRC-B fixture (1109 bytes repeating 0 to 254,
  CRC 2448192889) comes from SBB's `unit_test_redcrc.cc`.

The licence texts of those implementations follow.

## SBB RaSTA stack

<https://github.com/SchweizerischeBundesbahnen/sbb-rasta-stack>, revision
`81f2a8fa11285abcf83d2486e15d11081bf74224`, file `LICENSES/MIT.txt`:

```text
Copyright (C) 2022-2023 SBB AG, CH-3000 Bern

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Railway-CCS rasta-protocol and EULYNX Live librasta

<https://github.com/Railway-CCS/rasta-protocol>, revision
`aed9d83274eea0151aa12c9d84cb77bf1055d210`, and
<https://github.com/eulynx-live/librasta>, revision
`be81878aa56f19775c64065983f65dea4f1db9c8`, carry the same `LICENSE` file:

```text
MIT License

Copyright (c) 2021 Railway RaSTA

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
