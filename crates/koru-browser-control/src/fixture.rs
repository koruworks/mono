// SPDX-License-Identifier: MPL-2.0

//! The fixture site: the website every automated check and the
//! qualification's part A run against.
//!
//! **One definition, two carriers.** [`FixtureSite::respond`] is the whole
//! site: a request in, a response out, and a count kept per site and per
//! page. The suite calls it directly, in memory. The qualification program
//! serves the same function on two loopback ports. Nothing in this file
//! opens a port, and no test ever starts the second carrier.
//!
//! There are two sites. **Site A** has every page below. **Site B** has one
//! page, answers it for any path, and is not in the default grant: a request
//! counted on site B is a request that left site A.
//!
//! The site marks what must never be seen outside the browser: its session
//! cookie, two query values, a form field's value. It marks its page text
//! too, which may be seen in exactly one place.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

/// The value of the session cookie. Must appear nowhere Koru writes.
pub const MARK_COOKIE: &str = "MARK-COOKIE-7f3e9a";
/// The two query values of `/links`. They differ only here.
pub const MARK_QUERY_A: &str = "MARK-QUERY-A-51c2";
pub const MARK_QUERY_B: &str = "MARK-QUERY-B-88d0";
/// The value held by the field on `/form`. Must never be read.
pub const MARK_FIELD: &str = "MARK-FIELD-2b6d41";
/// Text on the private pages. May appear only in a tool reply.
pub const MARK_TEXT: &str = "MARK-TEXT-c09e17";
/// The code the second sign-in step asks for. The page shows it: it stands
/// for the number a person reads off their own device.
pub const SIGN_IN_CODE: &str = "246810";

const SESSION_COOKIE: &str = "fixture_session";
const STEP_COOKIE: &str = "fixture_step";
/// `/big` holds this much text.
pub const BIG_PAGE_BYTES: usize = 2 * 1024 * 1024;

/// The three seeded issues: a title and one sentence each.
pub const ISSUES: [(&str, &str); 3] = [
    (
        "The login page loads slowly",
        "Opening the login page takes four seconds on a cold start.",
    ),
    (
        "Export drops the last row",
        "A table exported to a file is missing its final row.",
    ),
    (
        "Dark theme hides the save button",
        "The save button has the same color as its background in the dark theme.",
    ),
];

/// Every page of site A.
pub const SITE_A_PAGES: [&str; 23] = [
    "/",
    "/login",
    "/login/code",
    "/issues",
    "/issues/1",
    "/issues/2",
    "/issues/3",
    "/links",
    "/out",
    "/redirect-out",
    "/script-out",
    "/refresh-out",
    "/form-out",
    "/popup",
    "/embeds",
    "/forge",
    "/slow",
    "/big",
    "/dialog",
    "/download",
    "/form",
    "/expire",
    "/favicon.ico",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Site {
    A,
    B,
}

/// One request, as either carrier hands it over.
#[derive(Debug, Clone, Copy)]
pub struct FixtureRequest<'a> {
    pub site: Site,
    /// `GET` or `POST`. Anything else is refused.
    pub method: &'a str,
    /// The path and, after a `?`, the query.
    pub target: &'a str,
    /// The `Cookie` header's value, or empty.
    pub cookies: &'a str,
    pub body: &'a [u8],
}

impl<'a> FixtureRequest<'a> {
    pub fn get(site: Site, target: &'a str) -> Self {
        Self {
            site,
            method: "GET",
            target,
            cookies: "",
            body: b"",
        }
    }

    pub fn with_cookies(mut self, cookies: &'a str) -> Self {
        self.cookies = cookies;
        self
    }

    pub fn posting(mut self, body: &'a [u8]) -> Self {
        self.method = "POST";
        self.body = body;
        self
    }
}

/// One response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureResponse {
    pub status: u16,
    /// Header names in the case they are sent in.
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    /// The response never finishes: a carrier sends the headers and the
    /// body so far, and then holds the connection open.
    pub never_finishes: bool,
}

impl FixtureResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Every `Set-Cookie` value.
    pub fn cookies_set(&self) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("Set-Cookie"))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap_or("")
    }
}

#[derive(Default)]
struct Counts {
    sites: BTreeMap<Site, u64>,
    pages: BTreeMap<(Site, String), u64>,
}

/// The site, bound to the two addresses it is reached at.
pub struct FixtureSite {
    a: String,
    b: String,
    counts: Mutex<Counts>,
}

impl FixtureSite {
    /// `a` and `b` are the two sites' addresses without a trailing slash,
    /// as the pages' links to each other spell them.
    pub fn new(a: &str, b: &str) -> Self {
        Self {
            a: a.trim_end_matches('/').to_owned(),
            b: b.trim_end_matches('/').to_owned(),
            counts: Mutex::new(Counts::default()),
        }
    }

    /// The site for the suite. The addresses are only text here: nothing
    /// listens on them.
    pub fn in_memory() -> Self {
        Self::new("http://127.0.0.1:41000", "http://127.0.0.1:41001")
    }

    pub fn address(&self, site: Site) -> &str {
        match site {
            Site::A => &self.a,
            Site::B => &self.b,
        }
    }

    /// How many requests a site has received.
    pub fn requests(&self, site: Site) -> u64 {
        let counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts.sites.get(&site).copied().unwrap_or(0)
    }

    /// How many requests one page has received, whatever their query.
    pub fn requests_for(&self, site: Site, path: &str) -> u64 {
        let counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts
            .pages
            .get(&(site, path.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    fn count(&self, site: Site, path: &str) {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        *counts.sites.entry(site).or_default() += 1;
        // Only the site's own pages are counted by name: a path is the
        // caller's text.
        let page = match site {
            Site::A if SITE_A_PAGES.contains(&path) => path,
            Site::A => "(other)",
            Site::B => "/",
        };
        *counts.pages.entry((site, page.to_owned())).or_default() += 1;
    }

    /// The whole site. Counts the request, then answers it.
    pub fn respond(&self, request: &FixtureRequest<'_>) -> FixtureResponse {
        let (path, _query) = request
            .target
            .split_once('?')
            .unwrap_or((request.target, ""));
        self.count(request.site, path);
        let post = match request.method {
            "GET" => false,
            "POST" => true,
            _ => return plain(405, "Method not allowed."),
        };
        if request.site == Site::B {
            return page(
                "Fixture site B",
                "<h1>Fixture site B</h1><p>This is the other site. It has one page.</p>",
            );
        }
        let signed_in = cookie(request.cookies, SESSION_COOKIE) == Some(MARK_COOKIE);
        let b = &self.b;
        match path {
            "/" => page(
                "Fixture site",
                "<h1>Fixture site</h1><p>A public page with links.</p><ul>\
                 <li><a href=\"/issues\">Issues</a></li>\
                 <li><a href=\"/login\">Sign in</a></li>\
                 <li><a href=\"/links\">Two links</a></li>\
                 <li><a href=\"/form\">A form</a></li>\
                 <li><a href=\"/out\">A link to the other site</a></li></ul>",
            ),
            "/login" if post => redirect(
                303,
                "/login/code",
                vec![format!(
                    "{STEP_COOKIE}=1; Path=/login; HttpOnly; SameSite=Lax"
                )],
            ),
            "/login" => page(
                "Sign in",
                "<h1>Sign in</h1><form method=\"post\" action=\"/login\">\
                 <label>Name <input name=\"name\" autocomplete=\"off\"></label>\
                 <button type=\"submit\">Continue</button></form>",
            ),
            "/login/code" => {
                if cookie(request.cookies, STEP_COOKIE) != Some("1") {
                    return redirect(303, "/login", Vec::new());
                }
                if !post {
                    return page(
                        "Enter the code",
                        &format!(
                            "<h1>Enter the code</h1><p>Your device shows {SIGN_IN_CODE}.</p>\
                             <form method=\"post\" action=\"/login/code\">\
                             <label>Code <input name=\"code\" autocomplete=\"off\"></label>\
                             <button type=\"submit\">Sign in</button></form>"
                        ),
                    );
                }
                if form_value(request.body, "code") != Some(SIGN_IN_CODE) {
                    return redirect(303, "/login/code", Vec::new());
                }
                redirect(
                    303,
                    "/issues",
                    vec![
                        format!(
                            "{SESSION_COOKIE}={MARK_COOKIE}; Path=/; Max-Age=86400; HttpOnly; SameSite=Lax"
                        ),
                        format!("{STEP_COOKIE}=; Path=/login; Max-Age=0"),
                    ],
                )
            }
            "/issues" | "/issues/1" | "/issues/2" | "/issues/3" if !signed_in => {
                redirect(302, "/login", Vec::new())
            }
            "/issues" => {
                let mut list = String::from("<h1>Issues</h1><ol>");
                for (index, (title, _)) in ISSUES.iter().enumerate() {
                    list.push_str(&format!(
                        "<li><a href=\"/issues/{}\">{title}</a></li>",
                        index + 1
                    ));
                }
                list.push_str(&format!("</ol><p>{MARK_TEXT}</p>"));
                page("Issues", &list)
            }
            "/issues/1" | "/issues/2" | "/issues/3" => {
                let index = usize::from(path.as_bytes()[8] - b'1');
                let (title, sentence) = ISSUES[index];
                page(
                    title,
                    &format!(
                        "<h1>{title}</h1><p>{sentence}</p><p>{MARK_TEXT}</p>\
                         <p><a href=\"/issues\">All issues</a></p>"
                    ),
                )
            }
            "/links" => page(
                "Two links",
                &format!(
                    "<h1>Two links</h1><p>They differ only in their query.</p>\
                     <p><a href=\"/issues/1?ref={MARK_QUERY_A}\">The first issue</a></p>\
                     <p><a href=\"/issues/1?ref={MARK_QUERY_B}\">The first issue</a></p>"
                ),
            ),
            "/out" => page(
                "A link out",
                &format!("<h1>A link out</h1><p><a href=\"{b}/?from=link\">The other site</a></p>"),
            ),
            "/redirect-out" => redirect(302, &format!("{b}/?from=redirect"), Vec::new()),
            "/script-out" => page(
                "A script navigation out",
                &format!(
                    "<h1>A script navigation out</h1>\
                     <script>location.href = \"{b}/?from=script\";</script>"
                ),
            ),
            "/refresh-out" => page(
                "A timed reload out",
                &format!(
                    "<meta http-equiv=\"refresh\" content=\"2; url={b}/?from=refresh\">\
                     <h1>A timed reload out</h1><p>This page leaves after two seconds.</p>"
                ),
            ),
            "/form-out" => page(
                "A form sent out",
                &format!(
                    "<h1>A form sent out</h1>\
                     <form id=\"out\" method=\"post\" action=\"{b}/?from=form\">\
                     <input type=\"hidden\" name=\"sent\" value=\"1\"></form>\
                     <script>document.getElementById(\"out\").submit();</script>"
                ),
            ),
            "/popup" => page(
                "A new tab out",
                &format!(
                    "<h1>A new tab out</h1>\
                     <p><a id=\"tab\" target=\"_blank\" href=\"{b}/?from=tab-link\">Open a tab</a></p>\
                     <script>window.open(\"{b}/?from=popup\");</script>"
                ),
            ),
            "/embeds" => page(
                "Embedded from the other site",
                &format!(
                    "<h1>Embedded from the other site</h1>\
                     <img alt=\"\" src=\"{b}/image?from=image\">\
                     <iframe title=\"frame\" src=\"{b}/?from=frame\"></iframe>"
                ),
            ),
            "/forge" => page(
                "Forged input",
                "<h1>Forged input</h1><p id=\"result\">waiting</p><script>\
                 document.dispatchEvent(new KeyboardEvent(\"keydown\", {key: \"a\", bubbles: true}));\
                 document.dispatchEvent(new MouseEvent(\"click\", {bubbles: true}));\
                 var called = typeof window.koruInputSeen === \"function\";\
                 if (called) { window.koruInputSeen(\"forged\"); }\
                 document.getElementById(\"result\").textContent = called ? \"binding called\" : \"no binding\";\
                 </script>",
            ),
            "/slow" => FixtureResponse {
                never_finishes: true,
                ..page(
                    "A page that never finishes",
                    "<h1>A page that never finishes</h1>",
                )
            },
            "/big" => {
                let line = "This is one line of a very long page of text. ";
                let mut text = String::with_capacity(BIG_PAGE_BYTES + 64);
                text.push_str("<h1>Two megabytes of text</h1><p>");
                while text.len() < BIG_PAGE_BYTES {
                    text.push_str(line);
                }
                text.push_str("</p>");
                page("Two megabytes of text", &text)
            }
            "/dialog" => page(
                "A dialog",
                "<h1>A dialog</h1><script>alert(\"Fixture dialog\");</script><p>After the dialog.</p>",
            ),
            "/download" => FixtureResponse {
                status: 200,
                headers: vec![
                    ("Content-Type", "application/octet-stream".to_owned()),
                    (
                        "Content-Disposition",
                        "attachment; filename=\"fixture-download.bin\"".to_owned(),
                    ),
                    ("Cache-Control", "no-store".to_owned()),
                ],
                body: b"fixture download\n".to_vec(),
                never_finishes: false,
            },
            "/form" => page(
                "A form",
                &format!(
                    "<h1>A form</h1><form><label>Token \
                     <input name=\"token\" value=\"{MARK_FIELD}\"></label>\
                     <textarea name=\"note\">{MARK_FIELD}</textarea></form>\
                     <p>The field above holds a value.</p>"
                ),
            ),
            "/expire" => redirect(
                303,
                "/",
                vec![format!("{SESSION_COOKIE}=; Path=/; Max-Age=0")],
            ),
            "/favicon.ico" => FixtureResponse {
                status: 204,
                headers: vec![("Cache-Control", "no-store".to_owned())],
                body: Vec::new(),
                never_finishes: false,
            },
            _ => plain(404, "No such page."),
        }
    }
}

fn page(title: &str, body: &str) -> FixtureResponse {
    FixtureResponse {
        status: 200,
        headers: vec![
            ("Content-Type", "text/html; charset=utf-8".to_owned()),
            ("Cache-Control", "no-store".to_owned()),
        ],
        body: format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <title>{title}</title></head><body>{body}</body></html>"
        )
        .into_bytes(),
        never_finishes: false,
    }
}

fn plain(status: u16, text: &str) -> FixtureResponse {
    FixtureResponse {
        status,
        headers: vec![
            ("Content-Type", "text/plain; charset=utf-8".to_owned()),
            ("Cache-Control", "no-store".to_owned()),
        ],
        body: text.as_bytes().to_vec(),
        never_finishes: false,
    }
}

fn redirect(status: u16, location: &str, cookies: Vec<String>) -> FixtureResponse {
    let mut headers = vec![
        ("Location", location.to_owned()),
        ("Cache-Control", "no-store".to_owned()),
    ];
    headers.extend(cookies.into_iter().map(|cookie| ("Set-Cookie", cookie)));
    FixtureResponse {
        status,
        headers,
        body: Vec::new(),
        never_finishes: false,
    }
}

/// One cookie's value from a `Cookie` header.
fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(known, _)| *known == name)
        .map(|(_, value)| value)
}

/// One field's value from a form body, when it needs no decoding.
fn form_value<'a>(body: &'a [u8], name: &str) -> Option<&'a str> {
    std::str::from_utf8(body)
        .ok()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(known, _)| *known == name)
        .map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(site: &FixtureSite, target: &str) -> FixtureResponse {
        site.respond(&FixtureRequest::get(Site::A, target))
    }

    /// The `Cookie` header a browser would send back after these responses.
    fn jar(responses: &[&FixtureResponse]) -> String {
        let mut jar: Vec<(String, String)> = Vec::new();
        for response in responses {
            for set in response.cookies_set() {
                let (pair, attributes) = set.split_once(';').unwrap_or((set, ""));
                let (name, value) = pair.split_once('=').unwrap();
                jar.retain(|(known, _)| known != name);
                if !attributes.contains("Max-Age=0") {
                    jar.push((name.to_owned(), value.to_owned()));
                }
            }
        }
        jar.iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Sign in by hand, as a person does: a name, then the code.
    fn sign_in(site: &FixtureSite) -> String {
        let first = site.respond(&FixtureRequest::get(Site::A, "/login").posting(b"name=person"));
        assert_eq!(
            (first.status, first.header("Location")),
            (303, Some("/login/code"))
        );
        let step = jar(&[&first]);
        let shown = site.respond(&FixtureRequest::get(Site::A, "/login/code").with_cookies(&step));
        assert_eq!(shown.status, 200);
        assert!(shown.text().contains(SIGN_IN_CODE));
        let code = format!("code={SIGN_IN_CODE}");
        let second = site.respond(
            &FixtureRequest::get(Site::A, "/login/code")
                .with_cookies(&step)
                .posting(code.as_bytes()),
        );
        assert_eq!(
            (second.status, second.header("Location")),
            (303, Some("/issues"))
        );
        jar(&[&first, &second])
    }

    #[test]
    fn m7_fixture_site_the_public_page_and_its_links() {
        let site = FixtureSite::in_memory();
        let home = get(&site, "/");
        assert_eq!(home.status, 200);
        assert_eq!(
            home.header("content-type"),
            Some("text/html; charset=utf-8")
        );
        assert!(home.text().contains("<title>Fixture site</title>"));
        for link in ["/issues", "/login", "/links", "/form", "/out"] {
            assert!(home.text().contains(&format!("href=\"{link}\"")), "{link}");
        }
        // A query does not change which page answers.
        assert_eq!(get(&site, "/?x=1").body, home.body);
        assert_eq!(get(&site, "/nothing").status, 404);
        assert_eq!(get(&site, "/favicon.ico").status, 204);
        let put = FixtureRequest {
            method: "PUT",
            ..FixtureRequest::get(Site::A, "/")
        };
        assert_eq!(site.respond(&put).status, 405);
        // Nothing is cached, so a request counted is a request made.
        for path in SITE_A_PAGES {
            let response = get(&site, path);
            assert_eq!(response.header("Cache-Control"), Some("no-store"), "{path}");
        }
    }

    #[test]
    fn m7_fixture_site_the_two_step_sign_in_sets_a_marked_cookie() {
        let site = FixtureSite::in_memory();
        assert!(get(&site, "/login").text().contains("name=\"name\""));
        // The second step is not reachable without the first.
        let early = get(&site, "/login/code");
        assert_eq!(
            (early.status, early.header("Location")),
            (303, Some("/login"))
        );
        let code = format!("code={SIGN_IN_CODE}");
        let skipped =
            site.respond(&FixtureRequest::get(Site::A, "/login/code").posting(code.as_bytes()));
        assert!(skipped.cookies_set().is_empty());
        // A wrong code signs nobody in.
        let first = site.respond(&FixtureRequest::get(Site::A, "/login").posting(b"name=person"));
        let step = jar(&[&first]);
        let wrong = site.respond(
            &FixtureRequest::get(Site::A, "/login/code")
                .with_cookies(&step)
                .posting(b"code=000000"),
        );
        assert_eq!(wrong.header("Location"), Some("/login/code"));
        assert!(wrong.cookies_set().is_empty());
        // The right one sets the marked session cookie, kept across a
        // restart of the browser and hidden from page script.
        let cookies = sign_in(&site);
        assert_eq!(cookies, format!("fixture_session={MARK_COOKIE}"));
        let site = FixtureSite::in_memory();
        let first = site.respond(&FixtureRequest::get(Site::A, "/login").posting(b"name=person"));
        let second = site.respond(
            &FixtureRequest::get(Site::A, "/login/code")
                .with_cookies(&jar(&[&first]))
                .posting(code.as_bytes()),
        );
        let session = second
            .cookies_set()
            .into_iter()
            .find(|set| set.starts_with("fixture_session="))
            .unwrap();
        for attribute in ["Path=/;", "Max-Age=86400", "HttpOnly", "SameSite=Lax"] {
            assert!(session.contains(attribute), "{attribute}");
        }
    }

    #[test]
    fn m7_fixture_site_the_private_read_needs_the_cookie() {
        let site = FixtureSite::in_memory();
        for path in ["/issues", "/issues/1", "/issues/2", "/issues/3"] {
            let refused = get(&site, path);
            assert_eq!(
                (refused.status, refused.header("Location")),
                (302, Some("/login"))
            );
            assert!(!refused.text().contains(MARK_TEXT));
            let forged = site
                .respond(&FixtureRequest::get(Site::A, path).with_cookies("fixture_session=guess"));
            assert_eq!(forged.status, 302, "{path}");
        }
        let cookies = sign_in(&site);
        let list = site.respond(&FixtureRequest::get(Site::A, "/issues").with_cookies(&cookies));
        assert_eq!(list.status, 200);
        assert!(list.text().contains(MARK_TEXT));
        for (index, (title, sentence)) in ISSUES.iter().enumerate() {
            let path = format!("/issues/{}", index + 1);
            assert!(
                list.text()
                    .contains(&format!("<a href=\"{path}\">{title}</a>"))
            );
            let issue = site.respond(&FixtureRequest::get(Site::A, &path).with_cookies(&cookies));
            assert_eq!(issue.status, 200);
            assert!(issue.text().contains(&format!("<title>{title}</title>")));
            assert!(issue.text().contains(sentence) && issue.text().contains(MARK_TEXT));
        }
        assert_eq!(
            site.respond(&FixtureRequest::get(Site::A, "/issues/4").with_cookies(&cookies))
                .status,
            404
        );
        // `/expire` ends the session.
        let expired = site.respond(&FixtureRequest::get(Site::A, "/expire").with_cookies(&cookies));
        assert_eq!(
            (expired.status, expired.header("Location")),
            (303, Some("/"))
        );
        let after = jar(&[
            &FixtureResponse {
                headers: vec![(
                    "Set-Cookie",
                    format!("fixture_session={MARK_COOKIE}; Path=/"),
                )],
                ..expired.clone()
            },
            &expired,
        ]);
        assert_eq!(after, "");
        assert_eq!(
            site.respond(&FixtureRequest::get(Site::A, "/issues").with_cookies(&after))
                .status,
            302
        );
    }

    #[test]
    fn m7_fixture_site_two_links_differ_only_in_a_marked_query() {
        let site = FixtureSite::in_memory();
        let links = get(&site, "/links");
        let text = links.text();
        let first = format!("<a href=\"/issues/1?ref={MARK_QUERY_A}\">The first issue</a>");
        let second = format!("<a href=\"/issues/1?ref={MARK_QUERY_B}\">The first issue</a>");
        assert!(text.contains(&first) && text.contains(&second));
        assert_eq!(first.replace(MARK_QUERY_A, MARK_QUERY_B), second);
        assert_ne!(MARK_QUERY_A, MARK_QUERY_B);
        // Either one reaches the same page.
        let cookies = sign_in(&site);
        let a = format!("/issues/1?ref={MARK_QUERY_A}");
        let b = format!("/issues/1?ref={MARK_QUERY_B}");
        let one = site.respond(&FixtureRequest::get(Site::A, &a).with_cookies(&cookies));
        let other = site.respond(&FixtureRequest::get(Site::A, &b).with_cookies(&cookies));
        assert_eq!(one, other);
        assert_eq!(site.requests_for(Site::A, "/issues/1"), 2);
    }

    #[test]
    fn m7_fixture_site_every_way_out_names_site_b() {
        let site = FixtureSite::in_memory();
        let b = site.address(Site::B).to_owned();
        assert_eq!(b, "http://127.0.0.1:41001");
        assert!(
            get(&site, "/out")
                .text()
                .contains(&format!("<a href=\"{b}/?from=link\">"))
        );
        let redirect = get(&site, "/redirect-out");
        assert_eq!(redirect.status, 302);
        assert_eq!(
            redirect.header("Location"),
            Some(format!("{b}/?from=redirect").as_str())
        );
        assert!(
            get(&site, "/script-out")
                .text()
                .contains(&format!("location.href = \"{b}/?from=script\";"))
        );
        assert!(get(&site, "/refresh-out").text().contains(&format!(
            "<meta http-equiv=\"refresh\" content=\"2; url={b}/?from=refresh\">"
        )));
        let form = get(&site, "/form-out");
        assert!(
            form.text()
                .contains(&format!("method=\"post\" action=\"{b}/?from=form\""))
        );
        assert!(form.text().contains(".submit();"));
        let popup = get(&site, "/popup");
        assert!(
            popup
                .text()
                .contains(&format!("window.open(\"{b}/?from=popup\");"))
        );
        assert!(popup.text().contains("target=\"_blank\""));
        let embeds = get(&site, "/embeds");
        assert!(
            embeds
                .text()
                .contains(&format!("<img alt=\"\" src=\"{b}/image?from=image\">"))
        );
        assert!(
            embeds
                .text()
                .contains(&format!("<iframe title=\"frame\" src=\"{b}/?from=frame\">"))
        );
        // None of this was a request to site B.
        assert_eq!(site.requests(Site::B), 0);
        assert_eq!(site.requests(Site::A), 7);
    }

    #[test]
    fn m7_fixture_site_site_b_has_one_page_and_counts_what_reaches_it() {
        let site = FixtureSite::in_memory();
        let home = site.respond(&FixtureRequest::get(Site::B, "/"));
        assert_eq!(home.status, 200);
        assert!(home.text().contains("<title>Fixture site B</title>"));
        // The same page for any path, a form sent to it included.
        for target in ["/?from=link", "/image?from=image", "/issues", "/login"] {
            assert_eq!(
                site.respond(&FixtureRequest::get(Site::B, target)).body,
                home.body
            );
        }
        let posted = site.respond(&FixtureRequest::get(Site::B, "/?from=form").posting(b"sent=1"));
        assert_eq!(posted.body, home.body);
        assert!(posted.cookies_set().is_empty());
        assert_eq!(site.requests(Site::B), 6);
        assert_eq!(site.requests_for(Site::B, "/"), 6);
        assert_eq!(site.requests(Site::A), 0);
        // Site A counts by page, and a path that is not a page as one.
        get(&site, "/issues/2?x=1");
        get(&site, "/issues/2");
        get(&site, "/secret-MARK-path");
        get(&site, "/another");
        assert_eq!(site.requests(Site::A), 4);
        assert_eq!(site.requests_for(Site::A, "/issues/2"), 2);
        assert_eq!(site.requests_for(Site::A, "(other)"), 2);
        assert_eq!(site.requests_for(Site::A, "/secret-MARK-path"), 0);
    }

    #[test]
    fn m7_fixture_site_the_pages_that_misbehave() {
        let site = FixtureSite::in_memory();
        // Never finishes.
        let slow = get(&site, "/slow");
        assert!(slow.never_finishes && slow.status == 200);
        for path in SITE_A_PAGES {
            assert_eq!(get(&site, path).never_finishes, path == "/slow", "{path}");
        }
        // Two megabytes of text.
        let big = get(&site, "/big");
        assert!(big.body.len() >= BIG_PAGE_BYTES && big.body.len() < BIG_PAGE_BYTES + 1024);
        assert!(big.text().ends_with("</p></body></html>"));
        // Opens a dialog.
        assert!(
            get(&site, "/dialog")
                .text()
                .contains("<script>alert(\"Fixture dialog\");</script>")
        );
        // A file.
        let download = get(&site, "/download");
        assert_eq!(
            download.header("Content-Type"),
            Some("application/octet-stream")
        );
        assert!(
            download
                .header("Content-Disposition")
                .unwrap()
                .starts_with("attachment;")
        );
        assert_eq!(download.body, b"fixture download\n");
        // A field holding a marked value, which is not the page's text.
        let form = get(&site, "/form");
        assert!(
            form.text()
                .contains(&format!("<input name=\"token\" value=\"{MARK_FIELD}\">"))
        );
        let outside_fields = form
            .text()
            .replace(&format!("value=\"{MARK_FIELD}\""), "")
            .replace(&format!(">{MARK_FIELD}</textarea>"), "></textarea>");
        assert!(!outside_fields.contains(MARK_FIELD));
        // A synthetic input event and a call to a binding by name.
        let forge = get(&site, "/forge");
        assert!(forge.text().contains("new KeyboardEvent(\"keydown\""));
        assert!(forge.text().contains("new MouseEvent(\"click\""));
        assert!(forge.text().contains("window.koruInputSeen(\"forged\")"));
    }

    #[test]
    fn m7_fixture_site_every_page_of_the_list_answers() {
        let site = FixtureSite::in_memory();
        let cookies = sign_in(&site);
        for path in SITE_A_PAGES {
            let step = format!("{cookies}; fixture_step=1");
            let response = site.respond(&FixtureRequest::get(Site::A, path).with_cookies(&step));
            let expected = match path {
                "/redirect-out" => 302,
                "/expire" => 303,
                "/favicon.ico" => 204,
                _ => 200,
            };
            assert_eq!(response.status, expected, "{path}");
            assert!(site.requests_for(Site::A, path) > 0, "{path}");
        }
        // The marks are where they belong and nowhere else.
        let public: Vec<&str> = SITE_A_PAGES
            .into_iter()
            .filter(|path| !path.starts_with("/issues"))
            .collect();
        for path in public {
            let response = get(&site, path);
            assert!(!response.text().contains(MARK_TEXT), "{path}");
            assert!(!response.text().contains(MARK_COOKIE), "{path}");
            assert!(
                response
                    .cookies_set()
                    .iter()
                    .all(|set| !set.contains(MARK_COOKIE)),
                "{path}"
            );
        }
        let marks = [
            MARK_COOKIE,
            MARK_QUERY_A,
            MARK_QUERY_B,
            MARK_FIELD,
            MARK_TEXT,
        ];
        for (index, mark) in marks.iter().enumerate() {
            assert!(mark.starts_with("MARK-"));
            assert!(marks.iter().skip(index + 1).all(|other| other != mark));
        }
    }
}
