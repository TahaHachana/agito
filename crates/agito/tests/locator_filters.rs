//! `filter_has`, `filter_has_not` and `filter_has_not_text`. Run with `--features integration`.
#![cfg(feature = "integration")]

use agito::{Page, expect};

const SHOP: &str = r#"
<ul>
  <li class="product">Desk lamp <span class="badge">Sold out</span></li>
  <li class="product">Coffee mug <button>Add to cart</button></li>
  <li class="product">Fountain pen <button>Add to cart</button></li>
  <li class="product">Gift card <button aria-label="Remove">x</button></li>
</ul>
<p id="added"></p>
<script>
  for (const b of document.querySelectorAll('button')) {
    b.onclick = () => document.querySelector('#added').textContent = b.closest('li').firstChild.textContent.trim();
  }
</script>"#;

#[agito::test]
async fn has_keeps_matches_containing_the_inner_locator(page: Page) -> agito::Result<()> {
    page.set_content(SHOP).await?;
    let products = page.locator(".product");

    let sold_out = products.filter_has(&page.locator(".badge"));
    expect(&sold_out).to_have_count(1).await?;
    expect(&sold_out).to_contain_text("Desk lamp").await?;

    let in_stock = products.filter_has_not(&page.locator(".badge"));
    expect(&in_stock).to_have_count(3).await?;

    // The inner locator can use any strategy: this one reads the accessibility tree.
    let buyable = products.filter_has(&page.get_by_role("button", Some("Add to cart")));
    expect(&buyable).to_have_count(2).await?;
    buyable.nth(1).locator("button")?.click().await?;
    expect(&page.locator("#added"))
        .to_have_text("Fountain pen")
        .await?;
    Ok(())
}

#[agito::test]
async fn has_not_text_is_the_negation_of_has_text(page: Page) -> agito::Result<()> {
    page.set_content(SHOP).await?;
    let products = page.locator(".product");
    expect(&products.filter_has_text("Sold out"))
        .to_have_count(1)
        .await?;
    expect(&products.filter_has_not_text("Sold out"))
        .to_have_count(3)
        .await?;
    expect(&products.filter_has_not_text("Sold out").first())
        .to_contain_text("Coffee mug")
        .await?;
    Ok(())
}

#[agito::test]
async fn an_element_does_not_contain_itself(page: Page) -> agito::Result<()> {
    page.set_content(r#"<ul id="outer"><li>one</li><li>two <ul><li>nested</li></ul></li></ul>"#)
        .await?;
    let items = page.locator("li");
    let with_sublist = items.filter_has(&page.locator("li"));
    expect(&with_sublist).to_have_count(1).await?;
    expect(&with_sublist).to_contain_text("nested").await?;
    expect(&items.filter_has_not(&page.locator("li")))
        .to_have_count(2)
        .await?;
    Ok(())
}

#[agito::test]
async fn matches_inside_shadow_roots_count(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<div class="card" id="a"><x-widget></x-widget></div>
        <div class="card" id="b"><span>plain</span></div>
        <script>
          customElements.define('x-widget', class extends HTMLElement {
            constructor() {
              super();
              this.attachShadow({ mode: 'open' }).innerHTML = '<button class="buy">Buy</button>';
            }
          });
        </script>"#,
    )
    .await?;
    let cards = page.locator(".card");
    let with_button = cards.filter_has(&page.locator(".buy"));
    expect(&with_button).to_have_count(1).await?;
    expect(&with_button).to_have_attribute("id", "a").await?;
    Ok(())
}

#[agito::test]
async fn an_inner_locator_from_another_frame_is_refused(page: Page) -> agito::Result<()> {
    page.set_content(r#"<ul><li>one</li></ul><iframe srcdoc="<b>inside</b>"></iframe>"#)
        .await?;
    let inner = page.frame_locator("iframe").locator("b");
    let err = page
        .locator("li")
        .filter_has(&inner)
        .count()
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("another page or frame"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[agito::test]
async fn filters_show_in_error_messages(page: Page) -> agito::Result<()> {
    page.set_content(SHOP).await?;
    let missing = page
        .locator(".product")
        .filter_has(&page.locator(".discount"))
        .with_timeout(std::time::Duration::from_millis(300));
    let err = missing.click().await.unwrap_err();
    assert!(
        err.to_string().contains(".product >> has=(.discount)"),
        "unexpected error: {err}"
    );
    Ok(())
}
