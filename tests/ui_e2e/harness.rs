//! Driving headless Chromium from tokio tests. rustwright's API blocks and
//! runs its own runtime, so a journey runs on a blocking thread while the
//! server keeps the test runtime's worker threads.

use std::time::{Duration, Instant};

use rustwright::{chromium, ActionOptions, Browser, GotoOptions, LaunchOptions, Page};

/// How long `wait_*` polls before failing.
const WAIT: Duration = Duration::from_secs(10);

/// One browser page on the console.
pub struct Ui {
    /// The server, `http://127.0.0.1:<port>`.
    pub base: String,
    /// The page.
    pub page: Page,
    // Declared after `page`, so the page is dropped before its browser
    // (Rust drops struct fields in declaration order).
    _browser: Browser,
}

/// Runs `journey` in a fresh headless Chromium, then fails it on any console
/// error (script errors and CSP violations are logged as errors by main.js).
/// Returns without running when no Chromium is installed.
///
/// # Returns
/// Whether the journey ran: `false` when it was skipped. A test that checks
/// server state the journey changed returns early on `false`, so it skips
/// too rather than failing on the unchanged state.
pub async fn drive<F>(base: String, journey: F) -> bool
where
    F: FnOnce(&Ui) + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        if chromium().executable_path().is_none() {
            eprintln!("skipping browser journey: no Chromium found (set RUSTWRIGHT_CHROMIUM)");
            return false;
        }
        let browser = chromium()
            .launch(LaunchOptions::default().headless(true))
            .expect("launch Chromium");
        let page = browser.new_page().expect("open a page");
        page.arm_console_capture().expect("capture the console");
        let ui = Ui {
            base,
            page,
            _browser: browser,
        };
        journey(&ui);
        ui.assert_no_console_errors();
        true
    })
    .await
    .expect("the journey thread panicked")
}

impl Ui {
    /// Navigates to `#<route>` (route like "/keys"; "" for the shell).
    pub fn open(&self, route: &str) {
        self.page
            .goto(
                &format!("{}/ui/#{route}", self.base),
                GotoOptions::default(),
            )
            .expect("navigate");
    }

    /// Signs in with `key` from the login card and waits for the shell.
    pub fn login(&self, key: &str) {
        self.open("");
        self.wait_visible("#login");
        self.fill("#login-form input[name=key]", key);
        self.click("#login-form button[type=submit]");
        self.wait_visible("#app");
    }

    fn poll(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}");
    }

    /// Waits until `selector` is visible.
    pub fn wait_visible(&self, selector: &str) {
        self.poll(selector, || self.page.is_visible(selector).unwrap_or(false));
    }

    /// Waits until `selector` is absent or hidden.
    pub fn wait_hidden(&self, selector: &str) {
        self.poll(selector, || {
            !self.page.is_visible(selector).unwrap_or(false)
        });
    }

    /// Waits until `selector`'s text contains `contains`, and returns the text.
    pub fn wait_text(&self, selector: &str, contains: &str) -> String {
        let mut last = String::new();
        self.poll(&format!("{selector} containing {contains:?}"), || {
            last = self.text(selector);
            last.contains(contains)
        });
        last
    }

    /// `selector`'s text content, or "" when it is absent.
    pub fn text(&self, selector: &str) -> String {
        self.page
            .text_content(selector, ActionOptions::default())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Clicks `selector` (auto-waits for it).
    pub fn click(&self, selector: &str) {
        self.page
            .click(selector, ActionOptions::default())
            .expect("click");
    }

    /// Fills the input `selector` with `value` (auto-waits for it).
    pub fn fill(&self, selector: &str, value: &str) {
        self.page
            .fill(selector, value, ActionOptions::default())
            .expect("fill");
    }

    /// Chooses `value` in the `<select>` `selector`, firing its `input` and
    /// `change` events. rustwright's `fill` refuses a `<select>`, and its
    /// `select_options` does not wait, so this polls until the select
    /// exists and has an option of that value (pickers fill theirs after an
    /// API call).
    pub fn select(&self, selector: &str, value: &str) {
        self.poll(&format!("{selector} offering {value:?}"), || {
            self.page.select_options(selector, &[value]).is_ok()
        });
    }

    /// Runs a JS function expression `(arg) => ...` in the page and returns
    /// its JSON result. The function form runs under the console's CSP.
    pub fn eval(&self, function: &str, arg: serde_json::Value) -> serde_json::Value {
        self.page
            .evaluate(function, Some(&arg), ActionOptions::default())
            .expect("evaluate")
    }

    /// Fails on any console record of type "error" since the page opened.
    /// Chromium's own "Failed to load resource" line for an HTTP error
    /// status is ignored: journeys provoke 401, 403 and 409 on purpose, and
    /// the console reports those through toasts, not script errors.
    pub fn assert_no_console_errors(&self) {
        let records = self
            .page
            .console_records(true, false)
            .expect("read the console");
        let errors: Vec<&str> = records
            .records
            .iter()
            .filter(|record| record.message_type == "error")
            .filter(|record| !record.text.starts_with("Failed to load resource"))
            .map(|record| record.text.as_str())
            .collect();
        assert!(errors.is_empty(), "console errors: {errors:?}");
    }
}
