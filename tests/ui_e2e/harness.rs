//! Driving headless Chromium from tokio tests. rustwright's API blocks and
//! runs its own runtime, so a journey runs on a blocking thread while the
//! server keeps the test runtime's worker threads.

use std::time::{Duration, Instant};

use rustwright::{chromium, ActionOptions, Browser, GotoOptions, LaunchOptions, Page};

/// How long `wait_*` polls before failing.
const WAIT: Duration = Duration::from_secs(10);

/// Holds the page's next request with method `arg.method` to a path ending
/// in `arg.suffix` until `window.releaseHeld()`; `window.heldDone` turns
/// true once that held answer has been handed back to the page, and
/// `window.heldMatches` counts the matching requests the page made while it
/// was held (the held one included). Runs through `ui.eval`, so it wraps
/// `fetch` without touching the page's code.
const HOLD_NEXT: &str = "(arg) => {
    const original = window.fetch;
    let release = null;
    window.heldDone = false;
    window.heldMatches = 0;
    window.releaseHeld = () => release();
    window.fetch = (input, init) => {
        const method = (init && init.method) || 'GET';
        const matches = method === arg.method && String(input).endsWith(arg.suffix);
        if (matches) window.heldMatches += 1;
        if (release === null && matches) {
            const held = new Promise(resolve => { release = resolve; });
            return held.then(() => original(input, init))
                .finally(() => { window.heldDone = true; window.fetch = original; });
        }
        return original(input, init);
    };
    return true;
}";

/// Answers the page's next request with method `arg.method` to a path
/// ending in `arg.suffix` with status `arg.status` and the error body
/// `{error, message: arg.message}`, without reaching the server (a refusal
/// or an outage the console must survive). Later requests go through. The
/// wrapper stays installed after it has answered (passing everything on),
/// so several of them nest: each answers its own request once.
const FAIL_NEXT: &str = "(arg) => {
    const original = window.fetch;
    let used = false;
    window.fetch = (input, init) => {
        const method = (init && init.method) || 'GET';
        if (!used && method === arg.method && String(input).endsWith(arg.suffix)) {
            used = true;
            return Promise.resolve(new Response(
                JSON.stringify({ error: 'test', message: arg.message }),
                { status: arg.status, headers: { 'Content-Type': 'application/json' } }));
        }
        return original(input, init);
    };
    return true;
}";

/// One browser page on the console.
pub struct Ui {
    /// The server, `http://127.0.0.1:<port>`.
    pub base: String,
    /// The page.
    pub page: Page,
    // Declared after `page`, so the page is dropped before its browser
    // (Rust drops struct fields in declaration order).
    browser: Browser,
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
            browser,
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

    /// Signs in on `/signin` with an email and password, and waits for the
    /// console to show who is signed in (its content, not only the shell:
    /// the shell is visible before `whoami` answers).
    pub fn sign_in_password(&self, email: &str, password: &str) {
        self.page
            .goto(&format!("{}/signin", self.base), GotoOptions::default())
            .expect("open the sign-in page");
        self.fill("#signin-email", email);
        self.fill("#signin-password", password);
        self.click("#signin-submit");
        self.wait_visible("#app");
        // `#whoami` reads "<user> - <level>" once the identity is loaded.
        self.wait_text("#whoami", " - ");
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

    /// Waits until `selector`'s text no longer contains `gone` (an absent
    /// element has no text, so that counts too).
    ///
    /// # Arguments
    /// * `selector` - the element to watch.
    /// * `gone` - the text that must disappear.
    ///
    /// # Panics
    /// When the text is still there after [`WAIT`].
    pub fn wait_text_gone(&self, selector: &str, gone: &str) {
        self.poll(&format!("{selector} without {gone:?}"), || {
            !self.text(selector).contains(gone)
        });
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

    /// Holds the page's next `method` request to a path ending in `suffix`
    /// until [`Ui::release_held`] (see [`HOLD_NEXT`]).
    ///
    /// # Arguments
    /// * `method` - the HTTP method, upper case.
    /// * `suffix` - the end of the request's URL.
    pub fn hold_next(&self, method: &str, suffix: &str) {
        self.eval(
            HOLD_NEXT,
            serde_json::json!({ "method": method, "suffix": suffix }),
        );
    }

    /// How many matching requests the page made since [`Ui::hold_next`],
    /// the held one included.
    pub fn held_matches(&self) -> u64 {
        self.eval("() => window.heldMatches", serde_json::json!(null))
            .as_u64()
            .unwrap_or(0)
    }

    /// Releases the held answer, waits until the page has it, then gives
    /// the page a moment to act on it (a stale drawer would be drawn by
    /// then).
    ///
    /// # Panics
    /// When the held answer has not arrived within [`WAIT`].
    pub fn release_held(&self) {
        self.eval("() => window.releaseHeld()", serde_json::json!(null));
        let deadline = Instant::now() + WAIT;
        while self.eval("() => window.heldDone", serde_json::json!(null)) != serde_json::json!(true)
        {
            assert!(Instant::now() < deadline, "the held answer never arrived");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    /// Makes the page's next `method` request to a path ending in `suffix`
    /// answer `status` with `message`, without reaching the server (see
    /// [`FAIL_NEXT`]).
    ///
    /// # Arguments
    /// * `method` - the HTTP method, upper case.
    /// * `suffix` - the end of the request's URL.
    /// * `status` - the status to answer, such as 409 or 503.
    /// * `message` - the error body's `message`.
    pub fn fail_next(&self, method: &str, suffix: &str, status: u16, message: &str) {
        self.eval(
            FAIL_NEXT,
            serde_json::json!({
                "method": method, "suffix": suffix, "status": status, "message": message,
            }),
        );
    }

    /// Gives the page a virtual WebAuthn authenticator (Chromium DevTools'
    /// `WebAuthn` domain): a platform authenticator that keeps discoverable
    /// credentials, verifies the user and needs no touch. It lives while the
    /// returned value does.
    ///
    /// # Panics
    /// If Chromium refuses a DevTools call.
    pub fn virtual_authenticator(&self) -> VirtualAuthenticator {
        let (socket, _) =
            tungstenite::connect(self.browser.ws_endpoint()).expect("connect to DevTools");
        let mut devtools = VirtualAuthenticator { socket, next_id: 0 };
        let attached = devtools.call(
            None,
            "Target.attachToTarget",
            serde_json::json!({ "targetId": self.page.target_id(), "flatten": true }),
        );
        let session = attached["sessionId"]
            .as_str()
            .expect("a DevTools session")
            .to_string();
        devtools.call(
            Some(&session),
            "WebAuthn.enable",
            serde_json::json!({ "enableUI": false }),
        );
        devtools.call(
            Some(&session),
            "WebAuthn.addVirtualAuthenticator",
            serde_json::json!({ "options": {
                "protocol": "ctap2",
                "transport": "internal",
                "hasResidentKey": true,
                "hasUserVerification": true,
                "isUserVerified": true,
                "automaticPresenceSimulation": true,
            }}),
        );
        devtools
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

/// A DevTools connection holding a virtual authenticator
/// ([`Ui::virtual_authenticator`]); dropping it closes the connection,
/// which removes the authenticator.
pub struct VirtualAuthenticator {
    socket: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    next_id: u64,
}

impl VirtualAuthenticator {
    /// Sends one DevTools command and waits for its answer, skipping the
    /// events that arrive meanwhile.
    ///
    /// # Panics
    /// If the connection fails or Chromium answers with an error.
    fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        self.next_id += 1;
        let mut command =
            serde_json::json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(session) = session {
            command["sessionId"] = serde_json::json!(session);
        }
        self.socket
            .send(tungstenite::Message::text(command.to_string()))
            .expect("send a DevTools command");
        loop {
            let message = self.socket.read().expect("read from DevTools");
            let Ok(text) = message.to_text() else {
                continue;
            };
            let Ok(reply) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            if reply["id"] == self.next_id {
                assert!(reply.get("error").is_none(), "{method}: {reply}");
                return reply["result"].clone();
            }
        }
    }
}
