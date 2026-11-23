use crate::error::ProviderError;
use crate::{invalid_input, validate_login, validate_path, validate_repo};
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

pub(super) const MAX_FILE_BYTES: usize = 64 * 1024;
pub(super) const MAX_PINS: usize = 8;
pub(super) const NAME_PATTERN: &str = r"^[a-z0-9][a-z0-9-]{0,62}$";

#[derive(Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct BotLogin(String);

impl TryFrom<String> for BotLogin {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let slug = value
            .strip_suffix("[bot]")
            .ok_or("expected an App bot login")?;
        validate_login(slug).map_err(|_| "invalid App bot login")?;
        Ok(Self(value))
    }
}

impl BotLogin {
    pub(super) fn matches(&self, login: &str) -> bool {
        self.0.eq_ignore_ascii_case(login)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Repo {
    pub(super) owner: String,
    pub(super) name: String,
}

impl Repo {
    pub(super) fn parse(value: &str) -> Result<Self, ProviderError> {
        let (owner, name) = value.split_once('/').ok_or_else(invalid_input)?;
        validate_login(owner)?;
        validate_repo(name)?;
        Ok(Self {
            owner: owner.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Semver,
    Image,
    Digest,
}

impl Kind {
    fn valid(&self, value: &str) -> bool {
        match self {
            Self::Semver => {
                let mut parts = value.split('.');
                (0..3).all(|_| {
                    parts.next().is_some_and(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
                }) && parts.next().is_none()
            }
            Self::Image => image_name(value).is_some(),
            Self::Digest => valid_digest(value),
        }
    }

    fn admits(&self, current: &str, next: &str) -> bool {
        match self {
            Self::Semver | Self::Digest => self.valid(current) && self.valid(next),
            Self::Image => image_name(current).is_some_and(|name| image_name(next) == Some(name)),
        }
    }
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    })
}

fn image_name(value: &str) -> Option<&str> {
    let (tagged, digest) = value.split_once('@')?;
    let (name, tag) = tagged.rsplit_once(':')?;
    let valid_name = name.split('/').all(|part| {
        !part.is_empty()
            && part.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'.' | b'_' | b'-' | b':')
            })
    });
    let valid_tag = (1..=128).contains(&tag.len())
        && tag
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    (valid_name && valid_tag && valid_digest(digest)).then_some(name)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSite {
    path: String,
    after: Option<String>,
    pattern: String,
}

pub(super) struct Site {
    pub(super) path: String,
    after: Option<String>,
    pattern: Regex,
}

impl TryFrom<RawSite> for Site {
    type Error = &'static str;

    fn try_from(raw: RawSite) -> Result<Self, Self::Error> {
        validate_path(&raw.path).map_err(|_| "invalid pin site path")?;
        if raw.path.is_empty() || raw.path.contains(['%', '\\']) {
            return Err("invalid pin site path");
        }
        if raw
            .after
            .as_ref()
            .is_some_and(|after| after.contains(['\r', '\n']))
            || raw.pattern.len() > 4096
            || raw.pattern.contains(['\r', '\n'])
        {
            return Err("invalid pin site line pattern");
        }
        let pattern = Regex::new(&format!(r"\A(?:{})\z", raw.pattern))
            .map_err(|_| "invalid pin site regex")?;
        if pattern.captures_len() != 2 {
            return Err("pin site regex must have exactly one capture group");
        }
        Ok(Self {
            path: raw.path,
            after: raw.after,
            pattern,
        })
    }
}

impl Site {
    fn capture<'a>(&self, line: &'a str, previous: Option<&str>) -> Option<regex::Match<'a>> {
        if self
            .after
            .as_deref()
            .is_some_and(|anchor| Some(anchor) != previous)
        {
            return None;
        }
        self.pattern.captures(line)?.get(1)
    }

    pub(super) fn matches_change<'a>(
        &self,
        before: Line<'_>,
        after: Line<'a>,
    ) -> Option<(&'a str, Range<usize>)> {
        let previous = before.previous.map(line_text);
        if self.after.is_some() && before.previous != after.previous {
            return None;
        }
        let old = self.capture(line_text(before.raw), previous)?;
        let new = self.capture(line_text(after.raw), after.previous.map(line_text))?;
        if old.start() != new.start()
            || before.raw.get(..old.start()) != after.raw.get(..new.start())
            || before.raw.get(old.end()..) != after.raw.get(new.end()..)
            || old.as_str() == new.as_str()
        {
            return None;
        }
        Some((new.as_str(), old.range()))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPin {
    repo: String,
    kind: Kind,
    sites: Vec<RawSite>,
}

pub(super) struct Pin {
    pub(super) repo: Repo,
    kind: Kind,
    pub(super) sites: Vec<Site>,
}

impl Pin {
    pub(super) fn check_value(&self, name: &str, value: &str) -> Result<(), ProviderError> {
        if value.len() > MAX_FILE_BYTES || !self.kind.valid(value) {
            return Err(invalid_value(name));
        }
        Ok(())
    }
}

#[derive(Default, Deserialize)]
#[serde(try_from = "BTreeMap<String, RawPin>")]
pub(crate) struct PinSet(pub(super) BTreeMap<String, Pin>);

impl PinSet {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<BTreeMap<String, RawPin>> for PinSet {
    type Error = &'static str;

    fn try_from(raw: BTreeMap<String, RawPin>) -> Result<Self, Self::Error> {
        let name_pattern = Regex::new(NAME_PATTERN).expect("fixed pin name regex");
        let mut pins = BTreeMap::new();
        for (name, raw) in raw {
            if !name_pattern.is_match(&name) || raw.sites.is_empty() {
                return Err("invalid pin name or missing pin sites");
            }
            let repo = Repo::parse(&raw.repo).map_err(|_| "invalid pin repository")?;
            let sites = raw
                .sites
                .into_iter()
                .map(Site::try_from)
                .collect::<Result<_, _>>()?;
            pins.insert(
                name,
                Pin {
                    repo,
                    kind: raw.kind,
                    sites,
                },
            );
        }
        Ok(Self(pins))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Line<'a> {
    pub(super) offset: usize,
    pub(super) raw: &'a str,
    pub(super) previous: Option<&'a str>,
}

pub(super) fn lines(text: &str) -> impl Iterator<Item = Line<'_>> {
    text.split_inclusive('\n').scan((0, None), |state, raw| {
        let (offset, previous) = *state;
        *state = (offset + raw.len(), Some(raw));
        Some(Line {
            offset,
            raw,
            previous,
        })
    })
}

fn line_text(raw: &str) -> &str {
    match raw.strip_suffix('\n') {
        Some(text) => text.strip_suffix('\r').unwrap_or(text),
        None => raw,
    }
}

pub(super) type Values = BTreeMap<String, String>;

pub(super) struct File {
    pub(super) sha: String,
    pub(super) text: String,
}

pub(super) type Files = BTreeMap<String, File>;

struct Edit<'a> {
    line: Line<'a>,
    range: Range<usize>,
    value: &'a str,
    site: &'a Site,
}

pub(super) fn rewrite(
    pins: &PinSet,
    files: &Files,
    values: &Values,
) -> Result<BTreeMap<String, String>, ProviderError> {
    let mut edits: BTreeMap<&str, Vec<Edit<'_>>> = BTreeMap::new();
    for (name, next) in values {
        let pin = &pins.0[name];
        let mut current = None;
        for site in &pin.sites {
            let file = files.get(&site.path).ok_or_else(|| no_match(name, site))?;
            let mut matched = false;
            for line in lines(&file.text) {
                let Some(capture) = site.capture(line_text(line.raw), line.previous.map(line_text))
                else {
                    continue;
                };
                let old = capture.as_str();
                matched = true;
                if current.replace(old).is_some_and(|value| value != old) {
                    return Err(ProviderError::new(
                        "pin-disagreement",
                        format!("pin {name} has inconsistent current values"),
                    ));
                }
                if !pin.kind.admits(old, next) {
                    return Err(invalid_value(name));
                }
                if old != next {
                    edits.entry(&site.path).or_default().push(Edit {
                        line,
                        range: capture.range(),
                        value: next,
                        site,
                    });
                }
            }
            if !matched {
                return Err(no_match(name, site));
            }
        }
    }

    let mut changed = BTreeMap::new();
    for (path, mut edits) in edits {
        edits.sort_unstable_by_key(|edit| edit.line.offset);
        let changed_lines: BTreeSet<_> = edits.iter().map(|edit| edit.line.offset).collect();
        let mut rendered = String::new();
        let mut cursor = 0;
        let text = &files[path].text;
        for edit in edits {
            let start = edit.line.offset + edit.range.start;
            let end = edit.line.offset + edit.range.end;
            if cursor > edit.line.offset
                || (edit.site.after.is_some()
                    && edit.line.previous.is_some_and(|previous| {
                        changed_lines.contains(&(edit.line.offset - previous.len()))
                    }))
            {
                return Err(unsafe_diff());
            }
            let new_line = format!(
                "{}{}{}",
                &edit.line.raw[..edit.range.start],
                edit.value,
                &edit.line.raw[edit.range.end..]
            );
            let after = Line {
                raw: &new_line,
                ..edit.line
            };
            if edit.site.matches_change(edit.line, after) != Some((edit.value, edit.range)) {
                return Err(unsafe_diff());
            }
            rendered.push_str(&text[cursor..start]);
            rendered.push_str(edit.value);
            cursor = end;
        }
        rendered.push_str(&text[cursor..]);
        if rendered.len() > MAX_FILE_BYTES {
            return Err(unsafe_diff());
        }
        changed.insert(path.to_owned(), rendered);
    }
    Ok(changed)
}

fn no_match(name: &str, site: &Site) -> ProviderError {
    ProviderError::new(
        "no-match",
        format!("pin {name} matched no line at {}", site.path),
    )
}

fn invalid_value(name: &str) -> ProviderError {
    ProviderError::new(
        "invalid-pin-value",
        format!("value for pin {name} does not match its kind"),
    )
}

pub(super) fn unsafe_diff() -> ProviderError {
    ProviderError::new(
        "unsafe-pin-diff",
        "the change is outside the configured pin captures",
    )
}
