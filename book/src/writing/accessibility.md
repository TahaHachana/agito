# Accessibility

agito can check a page the way assistive technology meets it. An audit reads the browser's
accessibility tree, the same tree a screen reader uses, and checks it against a set of rules
drawn from WCAG 2.1 levels A and AA.

## Auditing a page

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:accessibility_audit}}
```

`to_pass_accessibility_audit` fails on any error or warning, and its message lists each one
with the element at fault:

```text
Assertion to_pass_accessibility_audit failed: 2 violation(s) on https://shop.test/checkout:
  [error] a11y-empty-button: Button has no accessible name. Add text content, aria-label or aria-labelledby. (#close)
  [error] a11y-unlabeled-form-control: Form control (role: textbox) has no accessible name. ... (#card) in frame https://shop.test/payment
```

An audit runs once, when asked: it does not retry like other assertions. Wait for what the
test needs to be on the page first, as above.

To look at the findings instead of failing on them, run the audit yourself:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:accessibility_report}}
```

## What is checked

| Rule | Severity | Checks |
|---|---|---|
| `a11y-alt-text` | error | Images have a name: alt text or `aria-label`. Use `alt=""` for a decorative image, which leaves it out of the tree. |
| `a11y-unlabeled-form-control` | error | Text fields, checkboxes, radios, selects, sliders and switches have a label |
| `a11y-empty-button` | error | Buttons have a name, which an icon-only button often lacks |
| `a11y-empty-link` | error | Links have a name |
| `a11y-color-contrast` | error | Text stands out from its background: 4.5:1, or 3:1 for large text |
| `a11y-duplicate-id` | error | No `id` is used twice, since labels and `aria-*` references can only point at one |
| `a11y-missing-lang` | error | `<html>` has a `lang` attribute |
| `a11y-heading-hierarchy` | warning | Headings do not skip a level on the way down, such as an `h1` followed by an `h3` |
| `a11y-missing-landmark` | warning | The page has a `main` landmark |

**Contrast** is measured on what the page renders. For each element that holds visible
text, agito reads the text colour and the backgrounds behind it, from the element up through
its ancestors to the first opaque one, then blends any translucent layers over the white
canvas. Text on a background image or gradient, or in a colour format it cannot read, is
skipped rather than judged on a guess. Text in a disabled control is exempt, as WCAG allows.
Large text is at least 24px, or bold and at least 18.66px.

**Iframes** are audited too, each in its own document, cross-origin ones included, and a
violation in one names its frame. The two page-wide rules, `lang` and the `main` landmark,
apply only to the page's own document, since an embedded widget legitimately has neither.

## Options

```rust,ignore
use agito::accessibility::AuditOptions;

// Skip a rule, for a known problem you cannot fix yet.
expect(&page)
    .to_pass_accessibility_audit_with(&AuditOptions::skipping(&["a11y-color-contrast"]))
    .await?;

// Fail on errors only; warnings are still reported by audit_accessibility.
expect(&page)
    .to_pass_accessibility_audit_with(&AuditOptions {
        include_warnings: false,
        ..AuditOptions::default()
    })
    .await?;
```

An audit is a floor, not a certificate: automated rules catch the problems a machine can see,
such as missing names and poor contrast, but not whether a name makes sense or a page can be
used with a keyboard alone. [Role and accessible-name
assertions](assertions.md#roles-and-accessible-names) check individual elements precisely.

## Reading the tree

`page.accessibility_snapshot()` returns the tree itself: each node's role, name, value,
description and properties, with its children. Nodes the browser ignores, such as layout
wrappers, are left out, and their children take their place.

```rust,ignore
let snapshot = page.accessibility_snapshot().await?;
for node in snapshot.nodes() {
    println!("{} {:?}", node.role, node.name);
}
```

An iframe appears as a leaf; `frame.accessibility_snapshot()` reads its document.

## Writing a rule

A rule implements `AccessibilityRule`: an id, a description, and a `check` that receives one
audited document (its tree, its language, the ids used twice and its visible text samples)
and returns the violations in it. A [plugin](../extending.md) registers it, and from then on
every audit in the contexts the plugin loads into runs it:

```rust,ignore
use std::sync::Arc;

use agito::accessibility::{AccessibilityRule, AuditedDocument, Severity, Violation};

/// Pages need a <title>, which is the root node's name.
struct PageTitle;

impl AccessibilityRule for PageTitle {
    fn id(&self) -> &str {
        "house-page-title"
    }
    fn description(&self) -> &str {
        "Pages have a title."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        let titled = document.nodes().next().is_some_and(|root| !root.name.is_empty());
        if !document.is_top || titled {
            return Vec::new();
        }
        vec![Violation::new(self.id(), Severity::Error, "The page has no <title>.")]
    }
}

// In a plugin's on_loaded:
context.register_accessibility_rule(Arc::new(PageTitle));
```

A rule registered with a built-in rule's id replaces it, which is how to adjust a built-in
check for a project.
