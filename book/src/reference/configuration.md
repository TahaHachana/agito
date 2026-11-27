# Configuration

Settings come from three places, each overriding the one before:

1. `agito.config.json`, the first one found in the working directory or a directory above it.
   Tests run in their package's directory, so a file at the root of a workspace applies to
   every package in it.
2. `AGITO_*` environment variables.
3. Options set in code, such as [`ContextOptions`](../writing/fixtures.md#context-options) or
   [`LaunchOptions`](../scripting.md#launching).

A file that cannot be parsed is ignored with a warning, rather than failing every test.

## agito.config.json

```json
{
  "launch": {
    "headless": true,
    "channel": "chrome",
    "executablePath": null,
    "slowMo": 0,
    "timeout": 30000,
    "chromiumSandbox": false
  },
  "context": {
    "viewport": { "width": 1280, "height": 720 },
    "locale": "en-US",
    "colorScheme": "light",
    "ignoreHTTPSErrors": false,
    "baseURL": "http://localhost:3000"
  },
  "locator": { "timeout": 30000 },
  "assertions": { "timeout": 10000 },
  "failure": { "screenshot": true, "screenshotPath": "target/agito-failures" }
}
```

Every key is optional. Times are in milliseconds.

### launch

| Key | Variable | Default | Meaning |
|---|---|---|---|
| `headless` | `AGITO_HEADLESS` | `true` | Run without a window |
| `channel` | `AGITO_CHANNEL` | none | `chrome`, `edge` or `chromium`: which installed browser to prefer |
| `executablePath` | `AGITO_EXECUTABLE_PATH` | none | The browser executable to launch |
| `slowMo` | `AGITO_SLOWMO` | 0 | A pause before every command sent to the browser |
| `timeout` | `AGITO_LAUNCH_TIMEOUT` | 30000 | How long launching may take |
| `chromiumSandbox` | `AGITO_CHROMIUM_SANDBOX` | `false` | Run Chromium with its sandbox |

### context

| Key | Variable | Default | Meaning |
|---|---|---|---|
| `viewport` | | 1280 x 720 | `width` and `height` in CSS pixels |
| `locale` | `AGITO_LOCALE` | the browser's | Such as `fr-FR` |
| `colorScheme` | `AGITO_COLOR_SCHEME` | none | `light`, `dark` or `no-preference` |
| `ignoreHTTPSErrors` | `AGITO_IGNORE_HTTPS_ERRORS` | `false` | Accept invalid TLS certificates |
| `baseURL` | `AGITO_BASE_URL` | none | What relative URLs in `goto` resolve against |

### locator and assertions

| Key | Variable | Default | Meaning |
|---|---|---|---|
| `locator.timeout` | `AGITO_LOCATOR_TIMEOUT` | 30000 | How long an action waits for its element |
| `assertions.timeout` | `AGITO_ASSERTIONS_TIMEOUT` | 10000 | How long an `expect` assertion retries |

### failure

| Key | Variable | Default | Meaning |
|---|---|---|---|
| `screenshot` | `AGITO_FAILURES_SCREENSHOT` | `false` | Save a screenshot of the page when an `#[agito::test]` fails |
| `screenshotPath` | `AGITO_FAILURES_SCREENSHOT_PATH` | `target/agito-failures` | Where to save them, relative to the test's working directory |

## Other variables

| Variable | Meaning |
|---|---|
| `AGITO_MAX_CONTEXTS` | How many contexts a test binary keeps open at once (default 4), which limits how many browser tests run in parallel |
| `AGITO_RESULTS_DIR` | Where `#[agito::test]` writes its per-test results; `agito run` sets it |

Boolean variables accept `1`, `true`, `yes` and `on`, or `0`, `false`, `no` and `off`.
