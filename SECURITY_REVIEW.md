# Security And Safety Review

This note focuses on the security and safety posture of the current tool, not on navigation quality or product logic.

## Scope

The tool is intentionally designed to:

- use the user's current authenticated browser session
- keep data local
- avoid LLM calls and remote exfiltration
- extract and rank relevant page content from the live browser session

That product direction is valid. The main risks come from how browser control is achieved, not from the local embedding logic itself.

## Main Findings

### 1. Remote debugging on the user's real browser is a meaningful security exposure

The code connects to Chrome DevTools Protocol and, if needed, restarts the user's browser with `--remote-debugging-port=9222`.

Relevant code:

- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L175)
- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L194)
- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L255)
- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L291)

Why this matters:

- CDP gives broad control over the browser.
- The browser instance is the user's real profile with real cookies and active sessions.
- Another local process running as the same user may be able to connect to the debug endpoint and control the browser.
- That can expose authenticated pages, cookies, local/session storage, downloads, and browser actions.

Important nuance:

- "Data goes nowhere" is true for this app's business logic.
- It does not remove the local attack surface introduced by exposing the browser control channel.

### 2. Debug mode is left enabled after the tool exits

The current implementation disconnects from the browser but does not restore the browser to a normal non-debug run.

Relevant code:

- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L370)

Why this matters:

- If the tool restarted the browser into debug mode, that exposure can continue after the run completes.
- The browser may remain reachable on the debug port until the user fully quits and relaunches it normally.

### 3. Automatic restart into debug mode changes the user's browser security posture without an explicit opt-in

The code automatically restarts the browser if it cannot connect to a debuggable instance.

Relevant code:

- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L194)

Why this matters:

- This is a significant change to the user's primary browser environment.
- Even if the user wants authenticated-session support, the tool should make that state change explicit.

### 4. Cleanup is skipped on some error paths

If navigation fails, the main function returns early before tab cleanup and before the intended disconnect path.

Relevant code:

- [src/main.rs](/Users/shresthkapoor/code/dowsing/src/main.rs#L47)
- [src/main.rs](/Users/shresthkapoor/code/dowsing/src/main.rs#L57)
- [src/main.rs](/Users/shresthkapoor/code/dowsing/src/main.rs#L65)

Why this matters:

- Tabs opened by the tool may remain open.
- The browser session may not be disconnected cleanly.
- If debug mode had been enabled earlier, it may remain enabled until the user intervenes.

### 5. Clipboard export is a confidentiality risk, even if the app is fully local

The tool copies all extracted page content into the system clipboard.

Relevant code:

- [src/main.rs](/Users/shresthkapoor/code/dowsing/src/main.rs#L81)

Why this matters:

- The system clipboard is shared outside the app.
- Users can accidentally paste private content elsewhere.
- Clipboard history tools may retain the content.
- On Apple devices, Universal Clipboard may sync the content to another device.
- The tool itself may store nothing, but the clipboard can still leak sensitive content across app boundaries.

### 6. Linux process shutdown is broad and can affect more than intended

The Linux path uses `pkill -f` against the browser binary path.

Relevant code:

- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L283)

Why this matters:

- This can terminate more processes than intended.
- It is primarily a safety and reliability issue, but it still affects user trust and control.

### 7. Cross-domain exploration broadens the scope of what may be opened in the live authenticated browser

The navigator prefers same-domain links, but if there are none, it allows cross-domain exploration.

Relevant code:

- [src/navigator.rs](/Users/shresthkapoor/code/dowsing/src/navigator.rs#L124)

Why this matters:

- The browser session carries the user's real cookies and sessions.
- Cross-domain navigation increases the scope of sites the tool may open and inspect.
- That may be acceptable product behavior, but it should be intentional and visible to the user.

## Can The Tool Work Without Debug Mode?

Not in its current architecture.

The browser control is implemented through `chromiumoxide`, which uses Chrome DevTools Protocol.

Relevant code:

- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L393)
- [src/browser.rs](/Users/shresthkapoor/code/dowsing/src/browser.rs#L448)

Without a debuggable browser connection, the tool cannot:

- open tabs programmatically
- navigate pages
- wait for loads
- read page HTML from the live browser session
- close the tabs it opened

If authenticated-session access is a hard requirement, some privileged browser control path is required. In the current implementation, that path is CDP/debug mode.

## Is Debug Mode Always Unacceptable?

No. It may be an acceptable tradeoff under a narrow threat model:

- the user understands that their live browser is being exposed for local automation
- the machine is trusted
- there is no untrusted local software running as the same user
- the exposure is temporary and explicitly enabled
- the browser is restored to normal mode afterward

The current implementation does not yet contain that exposure tightly enough.

## Safer Approaches

These are listed in order of practical value for this product direction.

### 1. Require explicit opt-in before restarting the browser into debug mode

Instead of silently restarting the user's browser, require an explicit flag or confirmation.

Examples:

- `--allow-browser-restart-for-debugging`
- `--debug-port <port>`

Benefit:

- makes the trust tradeoff explicit

### 2. Use a random ephemeral debug port instead of fixed `9222`

Benefit:

- reduces discoverability by other local processes
- avoids conflicts with existing tooling

### 3. Restore the browser to normal mode when the run is complete

If the tool had to relaunch the browser with debugging enabled, it should offer to relaunch it normally afterward or provide a cleanup command.

Benefit:

- reduces the duration of exposure

### 4. Guarantee cleanup even on errors

Use a guard or structured shutdown path so cleanup and disconnect logic run whether navigation succeeds or fails.

Benefit:

- safer failure behavior

### 5. Make clipboard export opt-in

Possible options:

- `--copy-to-clipboard`
- `--output stdout`
- `--output file`

Benefit:

- reduces accidental data disclosure through the OS clipboard

### 6. Tighten browser shutdown logic on Linux

Avoid broad `pkill -f` matching. Prefer targeting a known PID or a more specific launch/cleanup strategy.

Benefit:

- reduces accidental process termination

### 7. Add explicit navigation bounds

Possible options:

- same-domain only by default
- allowlist of permitted domains
- prompt or log when crossing domains

Benefit:

- narrows browsing scope within the authenticated session

## Practical Recommendation

Given the product requirement that the tool must use the user's current authenticated session:

- keep browser automation
- keep local-only processing
- keep use of the live browser session

But harden the exposure:

1. require explicit opt-in for debug restart
2. use a random port instead of fixed `9222`
3. guarantee cleanup on error
4. return the browser to normal mode after the run
5. make clipboard export optional

## How To Check Whether The Browser Is In Debug Mode

Run:

```bash
curl http://127.0.0.1:9222/json/version
```

If the browser is listening on debug port `9222`, this returns JSON including fields such as:

- `Browser`
- `webSocketDebuggerUrl`

If it is not in debug mode on that port, you will usually get a connection error such as:

- connection refused
- failed to connect

You can also inspect process arguments:

```bash
ps aux | rg "remote-debugging-port|Chrome|Brave|Arc|Edge"
```

If you fully quit the browser and reopen it normally without the debug flag, debug mode should usually be gone.
