//! The `005 RPL_ISUPPORT` reply: what this particular network actually does.
//!
//! Almost nothing about IRC is fixed across networks. Which characters start a
//! channel name, which modes grant which prefix, how long a nick may be, and
//! how names are case-folded are all per-network facts announced in one or
//! more `005` replies during registration.
//!
//! A client that hardcodes the common answers works on Libera and breaks
//! somewhere else, usually in a way that corrupts the user list. Everything
//! here should be read from [`ISupport`] rather than assumed.

use std::collections::BTreeMap;

use crate::casemap::CaseMapping;
use crate::message::Message;

/// How a channel mode consumes parameters, which determines how to parse a
/// `MODE` change into individual mode events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    /// Type A: a list mode such as `+b`. Always takes a parameter.
    List,
    /// Type B: always takes a parameter, when set and when unset (e.g. `+k`).
    Setting,
    /// Type C: takes a parameter only when being set (e.g. `+l`).
    SettingOnSet,
    /// Type D: never takes a parameter (e.g. `+m`).
    Flag,
}

/// The four `CHANMODES` classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChanModes {
    pub list: String,
    pub setting: String,
    pub setting_on_set: String,
    pub flag: String,
}

impl Default for ChanModes {
    fn default() -> Self {
        // RFC 1459's modes, used when a server sends no CHANMODES token.
        ChanModes {
            list: "b".into(),
            setting: "k".into(),
            setting_on_set: "l".into(),
            flag: "imnpst".into(),
        }
    }
}

impl ChanModes {
    fn parse(value: &str) -> ChanModes {
        let mut parts = value.split(',');
        ChanModes {
            list: parts.next().unwrap_or_default().to_owned(),
            setting: parts.next().unwrap_or_default().to_owned(),
            setting_on_set: parts.next().unwrap_or_default().to_owned(),
            flag: parts.next().unwrap_or_default().to_owned(),
        }
    }

    /// Which class a mode character belongs to.
    pub fn kind(&self, mode: char) -> Option<ModeKind> {
        if self.list.contains(mode) {
            Some(ModeKind::List)
        } else if self.setting.contains(mode) {
            Some(ModeKind::Setting)
        } else if self.setting_on_set.contains(mode) {
            Some(ModeKind::SettingOnSet)
        } else if self.flag.contains(mode) {
            Some(ModeKind::Flag)
        } else {
            None
        }
    }
}

/// Everything the server told us about itself in its `005` replies.
#[derive(Debug, Clone)]
pub struct ISupport {
    raw: BTreeMap<String, Option<String>>,
    casemapping: CaseMapping,
    /// `(mode, prefix)` pairs in the order the server listed them, which is
    /// highest privilege first.
    prefixes: Vec<(char, char)>,
    chantypes: String,
    chanmodes: ChanModes,
    statusmsg: String,
}

impl Default for ISupport {
    fn default() -> Self {
        ISupport {
            raw: BTreeMap::new(),
            casemapping: CaseMapping::default(),
            prefixes: vec![('o', '@'), ('v', '+')],
            chantypes: "#&".into(),
            chanmodes: ChanModes::default(),
            statusmsg: String::new(),
        }
    }
}

impl ISupport {
    /// Applies one `005` reply.
    ///
    /// The first parameter is our own nick and the last is human-readable
    /// filler ("are supported by this server"); both are skipped. Servers send
    /// several of these, so this accumulates rather than replaces.
    pub fn ingest(&mut self, message: &Message) {
        let params = &message.params;
        if params.len() < 2 {
            return;
        }
        let tokens = &params[1..params.len() - 1];
        for token in tokens {
            self.apply_token(token);
        }
    }

    fn apply_token(&mut self, token: &str) {
        // A leading `-` withdraws a previously announced token.
        if let Some(key) = token.strip_prefix('-') {
            self.raw.remove(&key.to_ascii_uppercase());
            self.recompute(&key.to_ascii_uppercase(), None);
            return;
        }

        let (key, value) = match token.split_once('=') {
            Some((k, v)) => (k.to_ascii_uppercase(), Some(v.to_owned())),
            None => (token.to_ascii_uppercase(), None),
        };
        self.recompute(&key, value.as_deref());
        self.raw.insert(key, value);
    }

    /// Keeps the parsed hot-path fields in sync with the raw token map,
    /// falling back to the default when a token is withdrawn.
    fn recompute(&mut self, key: &str, value: Option<&str>) {
        let defaults = ISupport::default();
        match key {
            "CASEMAPPING" => {
                self.casemapping = value.map_or(defaults.casemapping, CaseMapping::parse)
            }
            "CHANTYPES" => {
                self.chantypes = value.map_or(defaults.chantypes, str::to_owned);
            }
            "CHANMODES" => {
                self.chanmodes = value.map_or(defaults.chanmodes, ChanModes::parse);
            }
            "STATUSMSG" => {
                self.statusmsg = value.unwrap_or_default().to_owned();
            }
            "PREFIX" => {
                self.prefixes = value.and_then(parse_prefix).unwrap_or(defaults.prefixes);
            }
            _ => {}
        }
    }

    /// The case-folding rule for nicks and channel names on this network.
    pub fn casemapping(&self) -> CaseMapping {
        self.casemapping
    }

    pub fn chanmodes(&self) -> &ChanModes {
        &self.chanmodes
    }

    /// The network's display name, from the `NETWORK` token.
    pub fn network(&self) -> Option<&str> {
        self.get("NETWORK")
    }

    /// The raw value of a token, if the server announced it with a value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw.get(key)?.as_deref()
    }

    /// Whether the server announced a token at all, with or without a value.
    pub fn has(&self, key: &str) -> bool {
        self.raw.contains_key(key)
    }

    /// A token's value parsed as a number, for limits like `NICKLEN`.
    pub fn get_usize(&self, key: &str) -> Option<usize> {
        self.get(key)?.parse().ok()
    }

    /// Whether `name` is a channel rather than a nick.
    pub fn is_channel(&self, name: &str) -> bool {
        name.chars()
            .next()
            .is_some_and(|c| self.chantypes.contains(c))
    }

    /// The prefix character a membership mode grants, e.g. `o` to `@`.
    pub fn prefix_for_mode(&self, mode: char) -> Option<char> {
        self.prefixes
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, p)| *p)
    }

    /// The membership mode a prefix character stands for, e.g. `@` to `o`.
    pub fn mode_for_prefix(&self, prefix: char) -> Option<char> {
        self.prefixes
            .iter()
            .find(|(_, p)| *p == prefix)
            .map(|(m, _)| *m)
    }

    /// Whether `mode` is a membership mode (one that grants a prefix) rather
    /// than a channel mode. `MODE` parsing has to know, because membership
    /// modes always take a nick parameter but are not listed in `CHANMODES`.
    pub fn is_membership_mode(&self, mode: char) -> bool {
        self.prefixes.iter().any(|(m, _)| *m == mode)
    }

    /// Sort rank of a prefix, lower being more privileged. Unprefixed users
    /// rank after every prefix, so this can order a channel's user list
    /// directly.
    pub fn prefix_rank(&self, prefix: char) -> usize {
        self.prefixes
            .iter()
            .position(|(_, p)| *p == prefix)
            .unwrap_or(self.prefixes.len())
    }

    /// Splits the leading membership prefixes off a `NAMES` entry, returning
    /// the prefixes and the bare nick.
    ///
    /// With the `multi-prefix` capability a server lists every prefix a user
    /// holds, so `@+alp` is one user with two of them.
    pub fn split_prefixes<'a>(&self, entry: &'a str) -> (&'a str, &'a str) {
        let end = entry
            .char_indices()
            .find(|(_, c)| self.mode_for_prefix(*c).is_none())
            .map_or(entry.len(), |(i, _)| i);
        entry.split_at(end)
    }

    /// Strips a `STATUSMSG` prefix such as the `@` in `@#channel`, which
    /// addresses only the operators of a channel. Returns the prefix and the
    /// channel name.
    pub fn split_statusmsg<'a>(&self, target: &'a str) -> (Option<char>, &'a str) {
        match target.chars().next() {
            Some(c) if self.statusmsg.contains(c) => (Some(c), &target[c.len_utf8()..]),
            _ => (None, target),
        }
    }

    /// Every announced token, for a debug view.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.raw.iter().map(|(k, v)| (k.as_str(), v.as_deref()))
    }
}

/// Parses a `PREFIX=(ov)@+` value into `(mode, prefix)` pairs.
///
/// Returns `None` for a malformed value so the caller can keep the defaults;
/// an empty value (`PREFIX=`) legitimately means the network has no membership
/// modes at all and yields an empty list.
fn parse_prefix(value: &str) -> Option<Vec<(char, char)>> {
    if value.is_empty() {
        return Some(Vec::new());
    }
    let body = value.strip_prefix('(')?;
    let (modes, prefixes) = body.split_once(')')?;
    if modes.chars().count() != prefixes.chars().count() {
        return None;
    }
    Some(modes.chars().zip(prefixes.chars()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isupport(tokens: &str) -> ISupport {
        let line = format!(":irc.libera.chat 005 alp {tokens} :are supported by this server");
        let mut s = ISupport::default();
        s.ingest(&Message::parse(&line).unwrap());
        s
    }

    #[test]
    fn defaults_are_usable_before_any_005_arrives() {
        let s = ISupport::default();
        assert_eq!(s.casemapping(), CaseMapping::Rfc1459);
        assert!(s.is_channel("#rust"));
        assert!(!s.is_channel("alp"));
        assert_eq!(s.prefix_for_mode('o'), Some('@'));
    }

    #[test]
    fn parses_a_realistic_libera_005() {
        let s = isupport(
            "CHANTYPES=# EXCEPTS INVEX CHANMODES=eIbq,k,flj,CFLMPQScgimnprstz \
             CHANLIMIT=#:250 PREFIX=(ov)@+ MAXLIST=bqeI:100 MODES=4 \
             NETWORK=Libera.Chat STATUSMSG=@+ CASEMAPPING=rfc1459 NICKLEN=16",
        );
        assert_eq!(s.network(), Some("Libera.Chat"));
        assert_eq!(s.get_usize("NICKLEN"), Some(16));
        assert_eq!(s.casemapping(), CaseMapping::Rfc1459);
        assert!(s.has("EXCEPTS"));
        assert_eq!(s.get("EXCEPTS"), None);

        // CHANTYPES=# means & is no longer a channel on this network.
        assert!(s.is_channel("#libera"));
        assert!(!s.is_channel("&local"));
    }

    #[test]
    fn chanmodes_classify_parameter_usage() {
        let s = isupport("CHANMODES=eIbq,k,flj,CFLMPQScgimnprstz");
        let m = s.chanmodes();
        assert_eq!(m.kind('b'), Some(ModeKind::List));
        assert_eq!(m.kind('k'), Some(ModeKind::Setting));
        assert_eq!(m.kind('l'), Some(ModeKind::SettingOnSet));
        assert_eq!(m.kind('m'), Some(ModeKind::Flag));
        assert_eq!(m.kind('Z'), None);
    }

    #[test]
    fn prefix_ranking_orders_the_user_list() {
        let s = isupport("PREFIX=(qaohv)~&@%+");
        assert_eq!(s.prefix_for_mode('q'), Some('~'));
        assert_eq!(s.mode_for_prefix('%'), Some('h'));
        assert!(s.is_membership_mode('h'));
        assert!(!s.is_membership_mode('b'));
        assert!(s.prefix_rank('~') < s.prefix_rank('@'));
        assert!(s.prefix_rank('@') < s.prefix_rank('+'));
        // An unprefixed user sorts last.
        assert!(s.prefix_rank('+') < s.prefix_rank(' '));
    }

    #[test]
    fn multi_prefix_names_entries_split_correctly() {
        let s = isupport("PREFIX=(qaohv)~&@%+");
        assert_eq!(s.split_prefixes("@+alp"), ("@+", "alp"));
        assert_eq!(s.split_prefixes("alp"), ("", "alp"));
        assert_eq!(s.split_prefixes("~&@%+alp"), ("~&@%+", "alp"));
        // A nick starting with a non-prefix character is untouched.
        assert_eq!(s.split_prefixes("[alp]"), ("", "[alp]"));
    }

    #[test]
    fn statusmsg_target_is_split_from_the_channel() {
        let s = isupport("STATUSMSG=@+ CHANTYPES=#");
        assert_eq!(s.split_statusmsg("@#chan"), (Some('@'), "#chan"));
        assert_eq!(s.split_statusmsg("#chan"), (None, "#chan"));
    }

    #[test]
    fn malformed_prefix_keeps_the_defaults() {
        // Mismatched lengths would otherwise pair modes with the wrong prefix
        // and silently mislabel every operator in the channel.
        let s = isupport("PREFIX=(ovh)@+");
        assert_eq!(s.prefix_for_mode('o'), Some('@'));
        assert_eq!(s.prefix_for_mode('v'), Some('+'));
        assert_eq!(s.prefix_for_mode('h'), None);
    }

    #[test]
    fn empty_prefix_means_no_membership_modes() {
        let s = isupport("PREFIX=");
        assert_eq!(s.prefix_for_mode('o'), None);
        assert_eq!(s.split_prefixes("@alp"), ("", "@alp"));
    }

    #[test]
    fn later_005_replies_accumulate() {
        let mut s = ISupport::default();
        for tokens in ["NETWORK=Libera.Chat", "NICKLEN=16 CASEMAPPING=ascii"] {
            let line = format!(":irc.libera.chat 005 alp {tokens} :are supported by this server");
            s.ingest(&Message::parse(&line).unwrap());
        }
        assert_eq!(s.network(), Some("Libera.Chat"));
        assert_eq!(s.get_usize("NICKLEN"), Some(16));
        assert_eq!(s.casemapping(), CaseMapping::Ascii);
    }

    #[test]
    fn negated_token_reverts_to_the_default() {
        let mut s = ISupport::default();
        let set = ":s 005 alp CASEMAPPING=ascii CHANTYPES=# :are supported by this server";
        s.ingest(&Message::parse(set).unwrap());
        assert_eq!(s.casemapping(), CaseMapping::Ascii);

        let unset = ":s 005 alp -CASEMAPPING :are supported by this server";
        s.ingest(&Message::parse(unset).unwrap());
        assert_eq!(s.casemapping(), CaseMapping::Rfc1459);
        assert!(!s.has("CASEMAPPING"));
        // The unrelated token survives.
        assert_eq!(s.get("CHANTYPES"), Some("#"));
    }

    #[test]
    fn short_005_is_ignored_rather_than_panicking() {
        let mut s = ISupport::default();
        s.ingest(&Message::parse(":s 005 alp").unwrap());
        s.ingest(&Message::parse(":s 005").unwrap());
        assert_eq!(s.network(), None);
    }
}
