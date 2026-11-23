//! Posts of X (spec 14.5). X draws its pages from data it loads (`/i/api/graphql/`), which holds
//! the full text of each post, the addresses behind its short `t.co` links, and the posts it
//! quotes; the page itself has the short links only, and no address for a quoted post. So a
//! page of X is read as the posts in that data: each is kept as a page of its own, at
//! `x.com/<user>/status/<id>`, from which the pages it links to and the post it quotes are read.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::collections::HashSet;

use serde_json::Value;

/// A post of X.
pub(crate) struct Post {
    pub id: String,
    /// The handle of its author, as `alexandr_wang`, and the name shown.
    pub user: String,
    pub name: String,
    /// When it was posted, as `2026-11-22T22:50:15Z`.
    pub time: String,
    /// Its text, with the short links as the addresses they stand for, and without the links of
    /// its images and of the post it quotes.
    pub text: String,
    /// The addresses it links to, in order, each once.
    pub links: Vec<String>,
    pub quoted: Option<Box<Post>>,
    /// The title and the start of the long article it is, when it is one.
    pub article: Option<(String, String)>,
    /// Its images and videos: the kind and the address of the image.
    pub media: Vec<(String, String)>,
}

impl Post {
    /// The address of the post.
    pub fn url(&self) -> String {
        format!("https://x.com/{}/status/{}", self.user, self.id)
    }
}

/// Whether an address is a page of X.
pub(crate) fn is_x(url: &url::Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("mobile."))
        .unwrap_or(&host);
    host == "x.com" || host == "twitter.com"
}

/// The post an address of X is the page of, as `.../status/<id>`.
pub(crate) fn status(url: &url::Url) -> Option<String> {
    if !is_x(url) {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    let at = segments.iter().position(|segment| *segment == "status")?;
    let id = segments.get(at + 1)?;
    (at >= 1 && !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| (*id).to_owned())
}

/// The posts in the data of a page, in the order they came, each once: those the page lists,
/// not those they quote. With `only`, the post of that ID alone, as the page of a post shows
/// its replies too.
pub(crate) fn posts(data: &[Value], only: Option<&str>) -> Vec<Post> {
    let mut found: Vec<Post> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for value in data {
        walk(value, &mut |tweet| {
            if let Some(post) = post(tweet)
                && only.is_none_or(|id| id == post.id)
                && seen.insert(post.id.clone())
            {
                found.push(post);
            }
        });
    }
    found
}

/// Finds the posts in a value, without going into the posts themselves.
fn walk(value: &Value, found: &mut dyn FnMut(&Value)) {
    match value {
        Value::Object(map) => {
            match map.get("__typename").and_then(Value::as_str) {
                Some("Tweet") if map.contains_key("rest_id") && map.contains_key("legacy") => {
                    found(value);
                    return;
                }
                Some("TweetWithVisibilityResults") => {
                    if let Some(tweet) = map.get("tweet") {
                        walk(tweet, found);
                    }
                    return;
                }
                _ => {}
            }
            for child in map.values() {
                walk(child, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, found);
            }
        }
        _ => {}
    }
}

/// A post from its data.
fn post(value: &Value) -> Option<Post> {
    let value = match value.get("__typename").and_then(Value::as_str) {
        Some("TweetWithVisibilityResults") => value.get("tweet")?,
        _ => value,
    };
    let legacy = value.get("legacy")?;
    // A repost is the post it reposts.
    if let Some(original) = legacy
        .pointer("/retweeted_status_result/result")
        .and_then(post)
    {
        return Some(original);
    }
    let id = value.get("rest_id")?.as_str()?.to_owned();
    let author = value.pointer("/core/user_results/result");
    let field = |name: &str| {
        author
            .and_then(|author| {
                author
                    .pointer(&format!("/core/{name}"))
                    .or_else(|| author.pointer(&format!("/legacy/{name}")))
            })
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    // X shows a post of an unknown author at `x.com/i/status/<id>`.
    let user = Some(field("screen_name"))
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| "i".to_owned());
    let note = value.pointer("/note_tweet/note_tweet_results/result");
    // A long post has its whole text in a note; the text of the post is its start.
    let (mut text, urls) = match note.and_then(|note| note.get("text").and_then(Value::as_str)) {
        Some(text) => (text.to_owned(), note.and_then(|note| note.pointer("/entity_set/urls"))),
        None => (
            unescape(
                legacy
                    .get("full_text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            legacy.pointer("/entities/urls"),
        ),
    };
    let quoted = value
        .pointer("/quoted_status_result/result")
        .and_then(post)
        .map(Box::new);
    let mut links: Vec<String> = Vec::new();
    for entity in urls.and_then(Value::as_array).into_iter().flatten() {
        let (Some(short), Some(address)) = (
            entity.get("url").and_then(Value::as_str),
            entity.get("expanded_url").and_then(Value::as_str),
        ) else {
            continue;
        };
        // The address of the quoted post is shown as the quote.
        let quoting = quoted.as_ref().is_some_and(|quoted| {
            url::Url::parse(address)
                .ok()
                .and_then(|address| status(&address))
                .is_some_and(|id| id == quoted.id)
        });
        if quoting {
            text = text.replace(short, "");
            continue;
        }
        text = text.replace(short, address);
        if !links.iter().any(|link| link == address) {
            links.push(address.to_owned());
        }
    }
    if let Some(short) = legacy
        .pointer("/quoted_status_permalink/url")
        .and_then(Value::as_str)
    {
        text = text.replace(short, "");
    }
    // A quoted post whose data is missing is read from its address.
    if quoted.is_none()
        && let Some(address) = legacy
            .pointer("/quoted_status_permalink/expanded")
            .and_then(Value::as_str)
        && !links.iter().any(|link| link == address)
    {
        links.push(address.to_owned());
    }
    let mut media: Vec<(String, String)> = Vec::new();
    for item in ["/extended_entities/media", "/entities/media"]
        .iter()
        .filter_map(|pointer| legacy.pointer(pointer).and_then(Value::as_array))
        .flatten()
    {
        if let Some(short) = item.get("url").and_then(Value::as_str) {
            text = text.replace(short, "");
        }
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("photo");
        if let Some(image) = item.get("media_url_https").and_then(Value::as_str)
            && !media.iter().any(|(_, known)| known == image)
        {
            media.push((kind.to_owned(), image.to_owned()));
        }
    }
    let article = value
        .pointer("/article/article_results/result")
        .map(|article| {
            let field = |name: &str| {
                article
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            (field("title"), field("preview_text"))
        });
    Some(Post {
        id,
        name: field("name"),
        user,
        time: iso_time(
            legacy
                .get("created_at")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        text: text.trim().to_owned(),
        links,
        quoted,
        article,
        media,
    })
}

/// The text of a post as X sends it, which has `&`, `<` and `>` as entities.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// A time as X writes it, `Sun Nov 22 22:50:15 +0000 2026`, as `2026-11-22T22:50:15Z`.
fn iso_time(time: &str) -> String {
    let parts: Vec<&str> = time.split_whitespace().collect();
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match parts.as_slice() {
        [_, month, day, clock, "+0000", year] => {
            match months.iter().position(|name| name == month) {
                Some(index) => format!(
                    "{year}-{:02}-{:0>2}T{clock}Z",
                    index + 1,
                    day
                ),
                None => time.to_owned(),
            }
        }
        _ => time.to_owned(),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The text of a post as HTML: its addresses as links, its lines as lines.
fn text_html(text: &str, links: &[String]) -> String {
    let mut html = escape(text);
    let mut ordered: Vec<&String> = links.iter().collect();
    ordered.sort_by_key(|link| std::cmp::Reverse(link.len()));
    // Each address is put in place once, so that one inside another stays as it is.
    for (index, link) in ordered.iter().enumerate() {
        html = html.replace(&escape(link), &format!("\u{0}{index}\u{0}"));
    }
    for (index, link) in ordered.iter().enumerate() {
        let link = escape(link);
        html = html.replace(
            &format!("\u{0}{index}\u{0}"),
            &format!("<a href=\"{link}\">{link}</a>"),
        );
    }
    html.replace('\n', "<br>\n")
}

/// The first line of a post, for its title.
fn headline(post: &Post) -> String {
    let line = post
        .text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .or_else(|| post.article.as_ref().map(|(title, _)| title.clone()))
        .unwrap_or_default();
    let mut short: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        short.push('…');
    }
    short
}

/// A post as a page of its own. It has nothing that changes as people react to the post, so
/// that the same post is the same page each time it is read.
pub(crate) fn render(post: &Post) -> String {
    let author = format!("{} (@{})", post.name, post.user);
    let mut html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{}</title></head><body><article>\n<header><p>{}</p><p><time datetime=\"{}\">{}</time></p></header>\n",
        escape(&format!("{author}: {}", headline(post))),
        escape(&author),
        escape(&post.time),
        escape(&post.time)
    );
    if !post.text.is_empty() {
        html.push_str(&format!("<p>{}</p>\n", text_html(&post.text, &post.links)));
    }
    if let Some((title, preview)) = &post.article {
        html.push_str(&format!(
            "<section><h2>{}</h2><p>{}</p></section>\n",
            escape(title),
            text_html(preview, &[])
        ));
    }
    if !post.media.is_empty() {
        html.push_str("<ul>");
        for (kind, image) in &post.media {
            let image = escape(image);
            html.push_str(&format!("<li><a href=\"{image}\">{}</a></li>", escape(kind)));
        }
        html.push_str("</ul>\n");
    }
    if let Some(quoted) = &post.quoted {
        let address = escape(&quoted.url());
        html.push_str(&format!(
            "<blockquote cite=\"{address}\"><p>{} (@{}) <a href=\"{address}\">{address}</a></p><p>{}</p></blockquote>\n",
            escape(&quoted.name),
            escape(&quoted.user),
            text_html(&quoted.text, &quoted.links)
        ));
    }
    let address = escape(&post.url());
    html.push_str(&format!(
        "<footer><p><a href=\"{address}\">{address}</a></p></footer>\n</article></body></html>\n"
    ));
    html
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tweet(id: &str, user: &str, text: &str, extra: Value) -> Value {
        let mut tweet = json!({
            "__typename": "Tweet",
            "rest_id": id,
            "core": { "user_results": { "result": { "core": { "screen_name": user, "name": user.to_uppercase() } } } },
            "legacy": { "full_text": text, "created_at": "Sun Nov 22 22:50:15 +0000 2026", "favorite_count": 5 },
        });
        if let (Value::Object(target), Value::Object(more)) = (&mut tweet, extra) {
            for (key, value) in more {
                if key == "legacy" {
                    for (name, field) in value.as_object().unwrap() {
                        target["legacy"][name] = field.clone();
                    }
                } else {
                    target.insert(key, value);
                }
            }
        }
        tweet
    }

    fn timeline(tweets: Vec<Value>) -> Value {
        json!({ "data": { "bookmark_timeline_v2": { "timeline": { "instructions": [{ "entries":
            tweets.into_iter().map(|tweet| json!({ "content": { "itemContent": { "tweet_results": { "result": tweet } } } })).collect::<Vec<_>>()
        }] } } } })
    }

    #[test]
    fn reads_the_posts_of_a_page_with_their_links_and_quotes() {
        let quoted = tweet("2", "bob", "引用された &amp; 投稿", json!({}));
        let first = tweet(
            "1",
            "alice",
            "見て（https://t.co/aaa）\n二行目 https://t.co/img https://t.co/qqq",
            json!({
                "legacy": {
                    "entities": {
                        "urls": [
                            { "url": "https://t.co/aaa", "expanded_url": "https://example.com/a?b=1&c=2" },
                            { "url": "https://t.co/qqq", "expanded_url": "https://x.com/bob/status/2" },
                        ],
                        "media": [{ "url": "https://t.co/img" }],
                    },
                    "extended_entities": { "media": [{ "url": "https://t.co/img", "type": "photo", "media_url_https": "https://pbs.twimg.com/media/p.jpg" }] },
                },
                "quoted_status_result": { "result": quoted },
            }),
        );
        let hidden = json!({ "__typename": "TweetWithVisibilityResults", "tweet": tweet("3", "carol", "短い", json!({
            "note_tweet": { "note_tweet_results": { "result": { "text": "長い全文 https://t.co/n", "entity_set": { "urls": [{ "url": "https://t.co/n", "expanded_url": "https://example.org/n" }] } } } },
        })) });
        let data = vec![timeline(vec![first, hidden]), timeline(vec![tweet("1", "alice", "again", json!({}))])];
        let found = posts(&data, None);
        assert_eq!(found.iter().map(|post| post.id.as_str()).collect::<Vec<_>>(), ["1", "3"]);
        let first = &found[0];
        assert_eq!(first.url(), "https://x.com/alice/status/1");
        assert_eq!(first.time, "2026-11-22T22:50:15Z");
        assert_eq!(first.text, "見て（https://example.com/a?b=1&c=2）\n二行目");
        assert_eq!(first.links, ["https://example.com/a?b=1&c=2"]);
        assert_eq!(first.media, [("photo".to_owned(), "https://pbs.twimg.com/media/p.jpg".to_owned())]);
        let quote = first.quoted.as_ref().expect("the quoted post");
        assert_eq!((quote.url(), quote.text.as_str()), ("https://x.com/bob/status/2".to_owned(), "引用された & 投稿"));
        assert_eq!(found[1].text, "長い全文 https://example.org/n");
        assert_eq!(found[1].links, ["https://example.org/n"]);
        // The page of a post is that post alone.
        assert_eq!(posts(&data, Some("3")).len(), 1);

        let html = render(first);
        assert!(html.contains("<title>ALICE (@alice): 見て（https://example.com/a?b=1&amp;c=2）</title>"));
        assert!(html.contains("<a href=\"https://example.com/a?b=1&amp;c=2\">https://example.com/a?b=1&amp;c=2</a>）<br>"));
        assert!(html.contains("<blockquote cite=\"https://x.com/bob/status/2\">"));
        assert!(!html.contains("t.co"));
        // Nothing that changes as people react to the post is in its page.
        let mut liked = data[0].clone();
        liked["data"]["bookmark_timeline_v2"]["timeline"]["instructions"][0]["entries"][0]["content"]["itemContent"]["tweet_results"]["result"]["legacy"]["favorite_count"] = json!(99);
        assert_eq!(html, render(&posts(&[liked], Some("1")).remove(0)));
    }

    #[test]
    fn knows_the_pages_of_posts() {
        let parse = |address: &str| url::Url::parse(address).unwrap();
        assert_eq!(status(&parse("https://x.com/bob/status/2124/photo/1")).as_deref(), Some("2124"));
        assert_eq!(status(&parse("https://twitter.com/i/web/status/77")).as_deref(), Some("77"));
        assert_eq!(status(&parse("https://x.com/i/bookmarks")), None);
        assert_eq!(status(&parse("https://example.com/bob/status/1")), None);
        assert!(is_x(&parse("https://mobile.twitter.com/home")));
    }
}
