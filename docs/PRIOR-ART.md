# Prior art and the code rule

BorderFit reimplements well-known Win32 techniques. Techniques and API sequences are not
copyrightable; code is. The rule for this repository:

- **Techniques yes, code no.** Do not copy or translate code from GPL projects into BorderFit,
  including "rewriting it in Rust". Link to the idea, write it from the Windows documentation.
- The GPL sources consulted during research were read for technique only and are not in this repo.
- UltraBorderless: README only, never its source.

| Project | License | Notes |
|---|---|---|
| [Techozu/BorderlessGaming](https://github.com/Techozu/BorderlessGaming) | The author's AutoHotkey script | BorderFit's predecessor and parity baseline |
| [Borderless Gaming](https://github.com/andrewmd5/Borderless-Gaming) | GPLv2 (legacy line, sunset 2025-09; v10 is paid) | Window picker model; injection-based extras in v10 |
| [UltraBorderless](https://github.com/Alpha-Leader/UltraBorderless) | GPL-3.0 (Rust/egui) | Region placement, profiles, cursor lock; README only |
| [SRWE](https://github.com/dtgDTGdtg/SRWE) | MIT (inactive) | Per-game resize experiments |
| Magpie, Lossless Scaling | various | Upscalers, complementary rather than competing |

Sources for the Windows behaviour BorderFit relies on are listed in
[ARCHITECTURE.md](ARCHITECTURE.md#windows-behaviour-borderfit-relies-on).
