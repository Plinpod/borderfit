# Security

Report vulnerabilities privately through GitHub's **Report a vulnerability** button on the
[Security tab](https://github.com/Plinpod/borderfit/security). You will get an answer within 72 hours.

Supported: the latest release, on Windows 10/11 x64.

Scope: BorderFit uses public Win32 window APIs only (no injection, no memory reads;
`OpenProcess` with `SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION` only) and makes no network
requests. Its hotkey is a plain `RegisterHotKey`; only when Windows refuses the chosen key (F12 is
reserved for debuggers) does it listen through a low-level keyboard hook in its own process, which
watches for that one key and nothing else. Every release publishes `SHA256SUMS.txt`.
