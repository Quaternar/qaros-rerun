//! The oldest QAROS runtime the viewer supports, checked against the version the runtime
//! reports at load.

/// Stream panels need a mixer that keeps working when a panel's stream restarts, which
/// QAROS 0.9.0-dev.3177 is the first build to do.
pub const MINIMUM_RUNTIME_VERSION: &str = "0.9.0-dev.3177";

/// Whether `version` precedes `minimum` in semantic version order, both written
/// `MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]`; `None` when either does not parse.
pub fn is_older_than(version: &str, minimum: &str) -> Option<bool> {
    Some(SemanticVersion::parse(version)? < SemanticVersion::parse(minimum)?)
}

/// Ordered as semantic versions are: the core numbers, then a pre-release before its release,
/// then the pre-release identifiers one by one, numeric ones before alphanumeric ones.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SemanticVersion {
    core: [u64; 3],
    is_release: bool,
    pre_release: Vec<Identifier>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    Numeric(u64),
    Alphanumeric(String),
}

impl SemanticVersion {
    fn parse(text: &str) -> Option<Self> {
        let without_build = text.trim().split('+').next()?;
        let (core_text, pre_release_text) = match without_build.split_once('-') {
            Some((core_text, pre_release_text)) => (core_text, Some(pre_release_text)),
            None => (without_build, None),
        };
        let mut numbers = core_text.split('.').map(|part| part.parse::<u64>().ok());
        let core = [numbers.next()??, numbers.next()??, numbers.next()??];
        if numbers.next().is_some() {
            return None;
        }
        let pre_release = match pre_release_text {
            Some(identifiers) => identifiers
                .split('.')
                .map(Identifier::parse)
                .collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        Some(Self {
            core,
            is_release: pre_release_text.is_none(),
            pre_release,
        })
    }
}

impl Identifier {
    fn parse(text: &str) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        Some(match text.parse::<u64>() {
            Ok(number) => Self::Numeric(number),
            Err(_) => Self::Alphanumeric(text.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_before_3177_are_older_and_later_ones_and_releases_are_not() {
        let older = |version| is_older_than(version, MINIMUM_RUNTIME_VERSION);
        assert_eq!(older("0.9.0-dev.3137"), Some(true));
        assert_eq!(older("0.8.4"), Some(true));
        assert_eq!(older("0.9.0-dev.3177"), Some(false));
        assert_eq!(older("0.9.0-dev.3200+abc123"), Some(false));
        assert_eq!(older("0.9.0-rc.2"), Some(false));
        assert_eq!(older("0.9.0"), Some(false));
        assert_eq!(older("0.10.0-dev.1"), Some(false));
    }

    #[test]
    fn a_version_that_does_not_parse_is_not_judged() {
        assert_eq!(is_older_than("unknown", MINIMUM_RUNTIME_VERSION), None);
        assert_eq!(is_older_than("0.9", MINIMUM_RUNTIME_VERSION), None);
        assert_eq!(is_older_than("0.9.0-", MINIMUM_RUNTIME_VERSION), None);
    }
}
