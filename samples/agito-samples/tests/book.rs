//! The examples in the agito book (`book/`). The book includes the code between the
//! `ANCHOR` markers, so every example it shows compiles and passes here.
//!
//! The examples drive a small demo shop at `https://shop.test`, which the [`DemoShop`] plugin
//! serves through a route on each test's context: nothing goes to the network.
#![cfg(feature = "integration")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agito::plugin::{LifecycleHook, Plugin, PluginContext};
use async_trait::async_trait;

use agito::storage::StorageState;
use agito::{
    Browser, BrowserContext, ColorScheme, ContextOptions, FilePayload, FulfillOptions, Page,
    Viewport, expect,
};
use serde_json::json;

const HOME: &str = r#"<!doctype html>
<title>Demo shop</title>
<header>
  <h1>Demo shop</h1>
  <nav><a href="/login">Sign in</a> <a href="/orders">Orders</a></nav>
</header>
<form action="/search">
  <label>Search <input name="q" placeholder="Search products"></label>
  <button>Search</button>
</form>
<ul id="featured">
  <li class="product" data-testid="product-lamp">Desk lamp <button>Add to cart</button></li>
  <li class="product" data-testid="product-mug">Coffee mug <button>Add to cart</button></li>
  <li class="product" data-testid="product-pen">Fountain pen <button>Add to cart</button>
    <span class="badge">Sold out</span></li>
</ul>
<p id="account">Not signed in</p>
<p>Cart: <span id="cart-count">0</span> items</p>
<button id="empty" onclick="if (confirm('Empty the cart?')) document.querySelector('#cart-count').textContent = '0'">Empty cart</button>
<p><a href="/report" target="_blank">Open report</a>
   <a download="orders.csv" href="data:text/csv,id%2Ctotal%0A1%2C42">Export orders</a></p>
<label>Avatar <input type="file" id="avatar"></label>
<script>
  const session = localStorage.getItem('session');
  if (session) document.querySelector('#account').textContent = `Signed in as ${session}`;
  for (const button of document.querySelectorAll('.product button')) {
    button.onclick = () => {
      const count = document.querySelector('#cart-count');
      count.textContent = Number(count.textContent) + 1;
    };
  }
</script>"#;

const LOGIN: &str = r#"<!doctype html>
<title>Sign in</title>
<form id="login">
  <label>Email <input type="email" name="email"></label>
  <label>Password <input type="password" name="password"></label>
  <label><input type="checkbox" name="remember"> Remember me</label>
  <label>Account <select name="account">
    <option value="personal">Personal</option>
    <option value="business">Business</option>
  </select></label>
  <button>Sign in</button>
</form>
<p id="message" role="status"></p>
<script>
  document.querySelector('#login').onsubmit = (event) => {
    event.preventDefault();
    const form = event.target;
    const ok = form.password.value === 'secret';
    if (ok) localStorage.setItem('session', form.email.value);
    document.querySelector('#message').textContent = ok
      ? `Welcome, ${form.email.value}`
      : 'Wrong password';
  };
</script>"#;

const ORDERS: &str = r#"<!doctype html>
<title>Orders</title>
<h1>Orders</h1>
<ul id="orders"></ul>
<script>
  fetch('/api/orders').then((r) => r.json()).then((orders) => {
    document.querySelector('#orders').innerHTML =
      orders.map((o) => `<li class="order">#${o.id}: ${o.total} EUR</li>`).join('');
  });
</script>"#;

const CHECKOUT: &str = r#"<!doctype html>
<title>Checkout</title>
<h1>Checkout</h1>
<iframe id="payment" src="/payment"></iframe>"#;

const PAYMENT: &str = r#"<!doctype html>
<label>Card number <input name="card"></label>
<button onclick="document.querySelector('.status').textContent = 'Paid'">Pay</button>
<p class="status"></p>"#;

const REPORT: &str = r#"<!doctype html><title>Report</title><h1>Sales report</h1>"#;

/// Serves the demo shop to every context in this test binary: the first page a context opens
/// adds the shop's route to it, so the examples need no setup of their own. A context route
/// also covers popups, from their first request.
#[agito::plugin]
#[derive(Default)]
struct DemoShop;

#[async_trait]
impl Plugin for DemoShop {
    fn id(&self) -> &str {
        "book.demo-shop"
    }

    async fn on_loaded(&self, context: &mut PluginContext) -> agito::Result<()> {
        // `on_loaded` runs once per context, so each context gets its own hook.
        context.register_lifecycle_hook(Arc::new(ServeShop::default()));
        Ok(())
    }
}

#[derive(Default)]
struct ServeShop {
    served: AtomicBool,
}

#[async_trait]
impl LifecycleHook for ServeShop {
    async fn on_page_created(&self, page: &Page) -> agito::Result<()> {
        if !self.served.swap(true, Ordering::SeqCst) {
            serve_shop(page.context()).await?;
        }
        Ok(())
    }
}

async fn serve_shop(context: &BrowserContext) -> agito::Result<()> {
    context
        .route("https://shop.test/**", |route| async move {
            let url = route.request().url().to_owned();
            let path = url.trim_start_matches("https://shop.test");
            let response = match path.split('?').next().unwrap_or("/") {
                "/" => FulfillOptions::html(HOME),
                "/login" => FulfillOptions::html(LOGIN),
                "/orders" => FulfillOptions::html(ORDERS),
                "/checkout" => FulfillOptions::html(CHECKOUT),
                "/payment" => FulfillOptions::html(PAYMENT),
                "/report" => FulfillOptions::html(REPORT),
                "/search" => {
                    let query = path.split("q=").nth(1).unwrap_or("").to_lowercase();
                    let hits: String = ["Desk lamp", "Coffee mug", "Fountain pen"]
                        .iter()
                        .filter(|p| p.to_lowercase().contains(&query))
                        .map(|p| format!("<li class=\"result\">{p}</li>"))
                        .collect();
                    FulfillOptions::html(format!(
                        "<!doctype html><title>Search</title><h1>Results for \"{query}\"</h1><ul>{hits}</ul>"
                    ))
                }
                "/api/orders" => FulfillOptions::json(&json!([{ "id": 1, "total": 42 }])),
                _ => FulfillOptions::text("not found").with_status(404),
            };
            let _ = route.fulfill(response).await;
        })
        .await?;
    Ok(())
}

// ANCHOR: first_test
#[agito::test]
async fn searching_finds_the_lamp(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/").await?;

    page.get_by_label("Search").fill("lamp").await?;
    page.get_by_role("button", Some("Search")).click().await?;

    expect(&page).to_have_url("**/search?q=lamp").await?;
    expect(&page.locator(".result"))
        .to_have_text("Desk lamp")
        .await?;
    Ok(())
}
// ANCHOR_END: first_test

#[agito::test]
async fn locators(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/").await?;

    // ANCHOR: locators_get_by
    let search = page.get_by_label("Search");
    let button = page.get_by_role("button", Some("Search"));
    let field = page.get_by_placeholder("Search products");
    let heading = page.get_by_text("Demo shop");
    let lamp = page.get_by_test_id("product-lamp");
    // ANCHOR_END: locators_get_by
    for locator in [&search, &button, &field, &heading, &lamp] {
        expect(locator).to_be_visible().await?;
    }

    // ANCHOR: locators_selectors
    let featured = page.locator("#featured .product"); // CSS, the default
    let by_xpath = page.locator("xpath=//ul[@id='featured']/li");
    let by_text = page.locator("text=Coffee mug");
    let by_role = page.locator("role=link[name=\"Sign in\"]");
    // ANCHOR_END: locators_selectors
    expect(&featured).to_have_count(3).await?;
    expect(&by_xpath).to_have_count(3).await?;
    expect(&by_text).to_be_visible().await?;
    expect(&by_role).to_be_visible().await?;

    // ANCHOR: locators_refine
    let products = page.locator(".product");
    let first = products.first();
    let mug = products.filter_has_text("mug");
    let third = products.nth(2);
    // `locator` on a locator searches inside it; it fails only on an empty selector.
    let add_mug = mug.locator("button")?;
    // ANCHOR_END: locators_refine
    expect(&first).to_contain_text("Desk lamp").await?;
    expect(&third).to_contain_text("Fountain pen").await?;
    add_mug.click().await?;
    expect(&page.locator("#cart-count"))
        .to_have_text("1")
        .await?;

    // ANCHOR: locators_filter
    let sold_out = products.filter_has(&page.get_by_text("Sold out"));
    let in_stock = products.filter_has_not(&page.get_by_text("Sold out"));
    let not_lamps = products.filter_has_not_text("lamp");
    // ANCHOR_END: locators_filter
    expect(&sold_out).to_contain_text("Fountain pen").await?;
    expect(&in_stock).to_have_count(2).await?;
    expect(&not_lamps).to_have_count(2).await?;

    // ANCHOR: locators_read
    let names = products.all_inner_texts().await?;
    let count = products.count().await?;
    let label = lamp.get_attribute("data-testid").await?;
    // ANCHOR_END: locators_read
    assert_eq!(count, 3);
    assert!(names[0].starts_with("Desk lamp"));
    assert_eq!(label.as_deref(), Some("product-lamp"));
    Ok(())
}

#[agito::test]
async fn actions(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/login").await?;

    // ANCHOR: actions_form
    page.get_by_label("Email").fill("ada@example.com").await?;
    page.get_by_label("Password").type_text("secret").await?;
    page.get_by_label("Remember me").check().await?;
    page.get_by_label("Account")
        .select_option(&["business"])
        .await?;
    page.get_by_label("Password").press("Enter").await?;
    // ANCHOR_END: actions_form
    expect(&page.get_by_role("status", None))
        .to_have_text("Welcome, ada@example.com")
        .await?;

    // ANCHOR: actions_timeout
    let err = page
        .locator("#does-not-exist")
        .with_timeout(Duration::from_millis(500))
        .click()
        .await
        .unwrap_err();
    assert!(err.is_timeout());
    // ANCHOR_END: actions_timeout
    Ok(())
}

#[agito::test]
async fn assertions(page: Page) -> agito::Result<()> {
    // ANCHOR: assertions
    let response = page
        .goto("https://shop.test/")
        .await?
        .expect("a document response");
    expect(&response).to_be_ok()?;

    expect(&page).to_have_title("Demo shop").await?;
    expect(&page.locator(".product")).to_have_count(3).await?;
    expect(&page.locator("#cart-count"))
        .to_have_text("0")
        .await?;

    page.get_by_test_id("product-pen")
        .locator("button")?
        .click()
        .await?;
    expect(&page.locator("#cart-count"))
        .to_have_text("1")
        .await?;
    expect(&page.get_by_text("Gift cards"))
        .not()
        .to_be_visible()
        .await?;
    // ANCHOR_END: assertions

    // ANCHOR: assertions_regex
    let count = regex::Regex::new(r"^\d+$").unwrap();
    expect(&page.locator("#cart-count"))
        .to_have_text(&count)
        .await?;
    // ANCHOR_END: assertions_regex

    // ANCHOR: assertions_options
    expect(&page.locator("h1"))
        .with_timeout(Duration::from_secs(2))
        .with_message("the shop's heading should show")
        .to_have_text("Demo shop")
        .await?;
    // ANCHOR_END: assertions_options
    Ok(())
}

#[agito::test]
async fn network(page: Page) -> agito::Result<()> {
    // ANCHOR: network_mock
    page.route("**/api/orders", |route| async move {
        let orders = json!([{ "id": 7, "total": 19 }, { "id": 8, "total": 5 }]);
        let _ = route.fulfill(FulfillOptions::json(&orders)).await;
    })
    .await?;

    page.goto("https://shop.test/orders").await?;
    expect(&page.locator(".order")).to_have_count(2).await?;
    // ANCHOR_END: network_mock

    // ANCHOR: network_wait
    let response = page.wait_for_response("**/api/orders", None); // listening starts here
    page.reload().await?;
    let orders: serde_json::Value = response.await?.json().await?;
    assert_eq!(orders[0]["id"], 7);
    // ANCHOR_END: network_wait

    // ANCHOR: network_observe
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    page.on_request(move |request| {
        log.lock()
            .unwrap()
            .push(format!("{} {}", request.method(), request.url()));
    });
    page.reload().await?;
    page.wait_for_network_idle(Duration::from_millis(300), None)
        .await?;
    // ANCHOR_END: network_observe
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|r| r.ends_with("/api/orders"))
    );
    Ok(())
}

#[agito::test]
async fn frames_and_popups(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/checkout").await?;

    // ANCHOR: frames
    let payment = page.frame_locator("#payment");
    payment
        .get_by_label("Card number")
        .fill("4242 4242 4242 4242")
        .await?;
    payment.get_by_role("button", Some("Pay")).click().await?;
    expect(&payment.locator(".status"))
        .to_have_text("Paid")
        .await?;
    // ANCHOR_END: frames

    page.goto("https://shop.test/").await?;
    // ANCHOR: popups
    let popup = page.wait_for_popup(None); // listening starts here
    page.get_by_text("Open report").click().await?;
    let report = popup.await?;
    expect(&report.locator("h1"))
        .to_have_text("Sales report")
        .await?;
    // ANCHOR_END: popups
    Ok(())
}

#[agito::test]
async fn files_and_dialogs(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/").await?;

    // ANCHOR: uploads
    let avatar = page.get_by_label("Avatar");
    avatar
        .set_input_files(FilePayload::new("me.png", b"not really a png".to_vec()))
        .await?;
    // ANCHOR_END: uploads
    let name: String = avatar
        .evaluate("function () { return this.files[0].name; }", &[])
        .await?;
    assert_eq!(name, "me.png");

    // ANCHOR: downloads
    let download = page.wait_for_download(None); // listening starts here
    page.get_by_text("Export orders").click().await?;
    let download = download.await?;
    assert_eq!(download.suggested_filename(), "orders.csv");
    let csv = std::fs::read_to_string(download.path().await?).unwrap();
    // ANCHOR_END: downloads
    assert!(csv.starts_with("id,total"));

    page.get_by_test_id("product-mug")
        .locator("button")?
        .click()
        .await?;
    // ANCHOR: dialogs
    page.on_dialog(|dialog| async move {
        assert_eq!(dialog.message, "Empty the cart?");
        let _ = dialog.accept(None).await;
    });
    page.get_by_role("button", Some("Empty cart"))
        .click()
        .await?;
    // ANCHOR_END: dialogs
    expect(&page.locator("#cart-count"))
        .to_have_text("0")
        .await?;
    Ok(())
}

// ANCHOR: context_options
fn dark_phone() -> ContextOptions {
    ContextOptions {
        viewport: Some(Viewport {
            width: 390,
            height: 844,
        }),
        color_scheme: Some(ColorScheme::Dark),
        locale: Some("fr-FR".into()),
        ..ContextOptions::default()
    }
}

#[agito::test(context = dark_phone)]
async fn the_shop_on_a_dark_phone(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/").await?;
    let width: u32 = page.evaluate("window.innerWidth").await?;
    let dark: bool = page
        .evaluate("matchMedia('(prefers-color-scheme: dark)').matches")
        .await?;
    assert_eq!(width, 390);
    assert!(dark);
    Ok(())
}
// ANCHOR_END: context_options

// ANCHOR: fixtures
#[agito::test]
async fn two_pages_share_a_context(context: BrowserContext, page: Page) -> agito::Result<()> {
    let other = context.new_page().await?;
    page.goto("https://shop.test/").await?;
    other.goto("https://shop.test/login").await?;
    assert_eq!(context.pages().len(), 2);
    Ok(())
}
// ANCHOR_END: fixtures

#[agito::test]
async fn signing_in_once(browser: Browser, page: Page) -> agito::Result<()> {
    // ANCHOR: storage_state
    // Sign in once...
    page.goto("https://shop.test/login").await?;
    page.get_by_label("Email").fill("ada@example.com").await?;
    page.get_by_label("Password").fill("secret").await?;
    page.get_by_role("button", Some("Sign in")).click().await?;
    expect(&page.get_by_role("status", None))
        .to_have_text("Welcome, ada@example.com")
        .await?;
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("signed-in.json");
    page.context().storage_state().await?.save(&file)?;

    // ...then start other contexts from where it left off.
    let state = StorageState::load(&file)?;
    let context = browser
        .new_context(ContextOptions {
            storage_state: Some(state),
            ..Default::default()
        })
        .await?;
    let shop = context.new_page().await?;
    shop.goto("https://shop.test/").await?;
    expect(&shop.locator("#account"))
        .to_have_text("Signed in as ada@example.com")
        .await?;
    // ANCHOR_END: storage_state
    context.close().await
}

// ANCHOR: retries
// Re-run up to twice if the browser is lost; a failed assertion is never re-run.
#[agito::test(retries = 2)]
async fn checkout_survives_a_browser_crash(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/checkout").await?;
    expect(&page.locator("h1")).to_have_text("Checkout").await
}
// ANCHOR_END: retries

// ANCHOR: quarantine
// Runs, but a failure is reported instead of failing the suite.
#[agito::test(quarantine = "flaky on CI, see issue 31")]
async fn recommendations_load(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/").await?;
    expect(&page.locator(".product")).to_have_count(3).await
}
// ANCHOR_END: quarantine
