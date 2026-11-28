//! Request mocking for the routing tools: a table of rules per context, consulted by one route
//! installed on the context once it has any.
//!
//! A pattern matches three ways: equal to the URL; with a `*`, as a glob covering the whole
//! URL, each `*` standing for any run of characters; otherwise, as a substring. When several
//! patterns match a request, the most recently registered handles it, and re-registering a
//! pattern replaces its rule in place.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agito::{BrowserContext, ContinueOptions, FulfillOptions, UrlMatcher};

/// What a rule does with the requests it matches.
#[derive(Debug, Clone)]
pub(crate) enum Rule {
    Fulfill {
        status: u16,
        body: String,
        content_type: Option<String>,
        headers: HashMap<String, String>,
    },
    Abort {
        error_code: Option<String>,
    },
    Continue {
        url: Option<String>,
        method: Option<String>,
        headers: HashMap<String, String>,
        post_data: Option<String>,
    },
}

impl Rule {
    pub fn kind(&self) -> &'static str {
        match self {
            Rule::Fulfill { .. } => "Fulfill",
            Rule::Abort { .. } => "Abort",
            Rule::Continue { .. } => "Continue",
        }
    }
}

/// Whether `pattern` matches `url`, by the rules in the module docs.
pub(crate) fn matches(pattern: &str, url: &str) -> bool {
    if pattern == url {
        return true;
    }
    if !pattern.contains('*') {
        return url.contains(pattern);
    }
    // A glob over the whole URL: the pieces between stars appear in order, the first at the
    // start and the last at the end.
    let pieces: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (pieces[0], pieces[pieces.len() - 1]);
    if !url.starts_with(first) || url.len() < first.len() + last.len() || !url.ends_with(last) {
        return false;
    }
    let mut rest = &url[first.len()..url.len() - last.len()];
    for piece in &pieces[1..pieces.len() - 1] {
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    true
}

/// One context's rules, in the order their patterns were first registered.
#[derive(Default)]
pub(crate) struct RouteTable {
    rules: Vec<(String, Rule)>,
    installed: bool,
}

impl RouteTable {
    /// The rule for `url`: the most recently registered pattern that matches.
    fn rule_for(&self, url: &str) -> Option<Rule> {
        self.rules
            .iter()
            .rev()
            .find(|(pattern, _)| matches(pattern, url))
            .map(|(_, rule)| rule.clone())
    }

    pub fn list(&self) -> Vec<(String, &'static str)> {
        self.rules
            .iter()
            .map(|(p, r)| (p.clone(), r.kind()))
            .collect()
    }

    /// Removes the rule for `pattern`, spelled as it was registered; whether there was one.
    pub fn remove(&mut self, pattern: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|(p, _)| p != pattern);
        self.rules.len() != before
    }
}

/// Registers `rule` for `pattern` on `context`, replacing the pattern's earlier rule in place,
/// and installs the context's route the first time.
pub(crate) async fn register(
    context: &BrowserContext,
    table: &Arc<Mutex<RouteTable>>,
    pattern: &str,
    rule: Rule,
) -> agito::Result<()> {
    let install = {
        let mut t = table.lock().unwrap();
        match t.rules.iter_mut().find(|(p, _)| p == pattern) {
            Some((_, existing)) => *existing = rule,
            None => t.rules.push((pattern.to_owned(), rule)),
        }
        !std::mem::replace(&mut t.installed, true)
    };
    if !install {
        return Ok(());
    }
    // The route intercepts only what some rule matches, so the rest of the page's traffic is
    // never held up.
    let matching = table.clone();
    let applying = table.clone();
    let result = context
        .route(
            UrlMatcher::Predicate(Arc::new(move |url| {
                matching.lock().unwrap().rule_for(url).is_some()
            })),
            move |route| {
                let table = applying.clone();
                async move {
                    let url = route.request().url().to_owned();
                    let rule = table.lock().unwrap().rule_for(&url);
                    let _ = match rule {
                        // The rule went between matching and handling.
                        None => route.continue_(ContinueOptions::default()).await,
                        Some(Rule::Fulfill {
                            status,
                            body,
                            content_type,
                            headers,
                        }) => {
                            route
                                .fulfill(FulfillOptions {
                                    status: Some(status),
                                    headers,
                                    content_type,
                                    body: body.into_bytes(),
                                    path: None,
                                })
                                .await
                        }
                        Some(Rule::Abort { error_code }) => {
                            route.abort(error_code.as_deref()).await
                        }
                        Some(Rule::Continue {
                            url,
                            method,
                            headers,
                            post_data,
                        }) => {
                            // Overrides add to the request's headers rather than replace them.
                            let headers = (!headers.is_empty()).then(|| {
                                let mut all = route.request().headers().clone();
                                all.extend(headers);
                                all
                            });
                            route
                                .continue_(ContinueOptions {
                                    url,
                                    method,
                                    headers,
                                    post_data,
                                })
                                .await
                        }
                    };
                }
            },
        )
        .await;
    if result.is_err() {
        table.lock().unwrap().installed = false;
    }
    result.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_exactly_as_globs_or_as_substrings() {
        let url = "https://example.com/api/status.json";
        assert!(matches(url, url));
        assert!(matches("*/api/status.json", url));
        assert!(matches("https://*.com/*", url));
        assert!(matches("*", url));
        assert!(matches("/api/", url));
        assert!(!matches("*/api/", url), "a glob covers the whole URL");
        assert!(!matches("/API/", url), "case-sensitive");
        assert!(!matches("https://*/other/*", url));
        assert!(matches("*status*json", url));
        assert!(!matches("*json*status", url));
    }

    #[test]
    fn the_newest_pattern_wins_and_replacing_keeps_the_place() {
        let mut table = RouteTable::default();
        let abort = Rule::Abort { error_code: None };
        let fulfill = Rule::Fulfill {
            status: 200,
            body: String::new(),
            content_type: None,
            headers: HashMap::new(),
        };
        table.rules.push(("/api/".into(), abort.clone()));
        table.rules.push(("*.json".into(), fulfill.clone()));
        let url = "https://x.test/api/a.json";
        assert_eq!(table.rule_for(url).unwrap().kind(), "Fulfill");
        // Re-registering /api/ replaces its rule in place: *.json still came later.
        table.rules[0].1 = fulfill;
        table.rules[1].1 = abort;
        assert_eq!(table.rule_for(url).unwrap().kind(), "Abort");
        assert!(table.remove("*.json"));
        assert!(!table.remove("*.json"));
        assert_eq!(table.list(), [("/api/".to_owned(), "Fulfill")]);
    }
}
