//! The licence gate: nothing is trained on unless its licence allows the
//! weights to be used commercially, and someone has checked that licence
//! at the source. The one exception is an internal-only model (`--internal`),
//! which may also learn from noncommercial data and is stamped so it never
//! ships in the public app.
//!
//! The manifest (`datasets.toml`) records each dataset's licence, who
//! verified it and when. [`problem`] refuses:
//!
//! * a licence not in [`ALLOWED`] (no-derivatives, PolyForm Noncommercial,
//!   unknown), and noncommercial ones ([`NONCOMMERCIAL`]) unless the model
//!   is internal-only;
//! * ShareAlike, unless explicitly allowed or the model is internal-only
//!   (whether trained weights are an adaptation is unsettled, and
//!   ShareAlike would bind their licence; an internal model isn't shared);
//! * anything not marked verified, with who and when;
//! * CC BY without the attribution line the model card must carry;
//! * by name, whatever the manifest says: the ChessQueries code, weights and
//!   SLCC data, licensed for noncommercial use only.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Licences whose terms let trained weights be used and shared
/// commercially (with attribution where required). `LicenseRef-esl-internal`
/// is our own data: photos players and staff sent, with consent.
pub const ALLOWED: &[&str] = &[
    "CC0-1.0",
    "CC-BY-4.0",
    "CC-BY-3.0",
    "CDLA-Permissive-2.0",
    "ODC-By-1.0",
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "LicenseRef-esl-internal",
    // docs/photo-licence.md: any model, commercial or not.
    "LicenseRef-open-chess-photo-licence-1",
    // docs/dataset-permissions.md: ChessReD's author granted commercial use,
    // crediting the dataset and the paper.
    "LicenseRef-chessred-permission-2026-10-14",
];
pub const SHARE_ALIKE: &[&str] = &["CC-BY-SA-4.0", "CC-BY-SA-3.0", "ODbL-1.0", "CDLA-Sharing-1.0"];
/// Allowed only for an internal-only model: used by our own staff, never
/// distributed (no derivatives licences stay refused: a model is one).
pub const NONCOMMERCIAL: &[&str] = &["CC-BY-NC-4.0", "CC-BY-NC-SA-4.0", "CC-BY-NC-3.0", "CC-BY-NC-SA-3.0"];
/// Refused whatever a manifest says.
pub const DENIED_NAMES: &[&str] = &["chessqueries", "chessq", "slcc"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dataset {
    #[serde(skip)]
    pub name: String,
    #[serde(default)]
    pub licence: String,
    #[serde(default)]
    pub licence_url: String,
    #[serde(default)]
    pub licence_verified: bool,
    #[serde(default)]
    pub verified_by: String,
    #[serde(default)]
    pub verified_on: String,
    #[serde(default)]
    pub attribution: String,
    /// `chessred`, `jsonl` or `photos` (unlabelled, for a teacher to read).
    #[serde(default = "jsonl")]
    pub format: String,
    /// Under the data root.
    #[serde(default)]
    pub path: String,
}

fn jsonl() -> String {
    "jsonl".into()
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    dataset: BTreeMap<String, Dataset>,
}

pub fn load_manifest(path: &Path) -> Result<BTreeMap<String, Dataset>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_manifest(&text).with_context(|| format!("parsing {}", path.display()))
}

/// The manifest that comes with this crate (`datasets.toml`).
pub const BUILT_IN: &str = include_str!("../datasets.toml");

/// A manifest's datasets, by name.
pub fn parse_manifest(text: &str) -> Result<BTreeMap<String, Dataset>> {
    let m: Manifest = toml::from_str(text)?;
    Ok(m.dataset
        .into_iter()
        .map(|(k, mut d)| {
            (k.clone(), {
                d.name = k;
                d
            })
        })
        .collect())
}

/// What a training run may use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policy {
    /// ShareAlike data in a model that may be shared (its licence binds).
    pub allow_share_alike: bool,
    /// An internal-only model: noncommercial and ShareAlike data too.
    pub internal: bool,
}

/// Whether `d` is only usable for an internal-only model.
pub fn needs_internal(d: &Dataset) -> bool {
    NONCOMMERCIAL.contains(&d.licence.trim())
}

/// Why `d` can't be trained on, or `None` if it can.
pub fn problem(d: &Dataset, policy: Policy) -> Option<String> {
    let lowered = format!("{} {}", d.name, d.path).to_lowercase();
    if DENIED_NAMES.iter().any(|n| lowered.contains(n)) {
        return Some(format!("{}: ChessQueries material is licensed for noncommercial use only", d.name));
    }
    let lic = d.licence.trim();
    if NONCOMMERCIAL.contains(&lic) {
        if !policy.internal {
            return Some(format!(
                "{}: {lic} doesn't allow commercial use of the weights; pass --internal for an internal-only model",
                d.name
            ));
        }
    } else if SHARE_ALIKE.contains(&lic) {
        if !(policy.allow_share_alike || policy.internal) {
            return Some(format!(
                "{}: {lic} is ShareAlike; pass --allow-share-alike if the weights may carry it",
                d.name
            ));
        }
    } else if !ALLOWED.contains(&lic) {
        let shown = if lic.is_empty() { "unknown" } else { lic };
        return Some(format!("{}: licence {shown} doesn't allow commercial use of the weights", d.name));
    }
    if !d.licence_verified {
        return Some(format!(
            "{}: licence {lic} hasn't been verified at the source (set licence_verified, verified_by, verified_on)",
            d.name
        ));
    }
    if d.verified_by.trim().is_empty() || d.verified_on.trim().is_empty() {
        return Some(format!("{}: say who verified the licence and when", d.name));
    }
    if lic.starts_with("CC-BY") && d.attribution.trim().is_empty() {
        return Some(format!("{}: {lic} needs an attribution line for the model card", d.name));
    }
    None
}

pub fn check(datasets: &[&Dataset], policy: Policy) -> Result<()> {
    let problems: Vec<String> = datasets.iter().filter_map(|d| problem(d, policy)).collect();
    if !problems.is_empty() {
        bail!("refusing to train:\n  {}", problems.join("\n  "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Dataset {
        Dataset {
            name: "photos".into(),
            licence: "CC-BY-4.0".into(),
            licence_verified: true,
            verified_by: "me".into(),
            verified_on: "2026-10-12".into(),
            attribution: "Photos, by someone".into(),
            path: "photos".into(),
            ..Dataset::default()
        }
    }

    const SHARED: Policy = Policy { allow_share_alike: false, internal: false };
    const SA: Policy = Policy { allow_share_alike: true, internal: false };
    const INTERNAL: Policy = Policy { allow_share_alike: false, internal: true };

    #[test]
    fn the_gate() {
        assert_eq!(problem(&ok(), SHARED), None);
        let nc = Dataset { licence: "CC-BY-NC-4.0".into(), ..ok() };
        assert!(problem(&nc, SHARED).unwrap().contains("--internal"));
        assert!(problem(&nc, SA).is_some());
        let unverified = Dataset { licence_verified: false, ..ok() };
        assert!(problem(&unverified, SHARED).unwrap().contains("verified"));
        let no_who = Dataset { verified_by: String::new(), ..ok() };
        assert!(problem(&no_who, SHARED).unwrap().contains("who"));
        let no_credit = Dataset { attribution: String::new(), ..ok() };
        assert!(problem(&no_credit, SHARED).unwrap().contains("attribution"));
        let sa = Dataset { licence: "CC-BY-SA-4.0".into(), ..ok() };
        assert!(problem(&sa, SHARED).is_some());
        assert_eq!(problem(&sa, SA), None);
        assert_eq!(problem(&sa, INTERNAL), None);
        let slcc = Dataset { name: "my-slcc-copy".into(), licence: "CC0-1.0".into(), ..ok() };
        assert!(problem(&slcc, SA).unwrap().contains("noncommercial"));
        let unknown = Dataset { licence: "UNVERIFIED".into(), ..ok() };
        assert!(problem(&unknown, INTERNAL).is_some());
        assert!(check(&[&ok(), &nc], SHARED).is_err());
    }

    #[test]
    fn internal_models_take_noncommercial_data_but_still_need_it_checked() {
        let nc_sa = Dataset { licence: "CC-BY-NC-SA-4.0".into(), ..ok() };
        assert_eq!(problem(&nc_sa, INTERNAL), None);
        assert!(needs_internal(&nc_sa) && !needs_internal(&ok()));
        let unverified = Dataset { licence_verified: false, ..nc_sa.clone() };
        assert!(problem(&unverified, INTERNAL).unwrap().contains("verified"));
        let no_credit = Dataset { attribution: String::new(), ..nc_sa };
        assert!(problem(&no_credit, INTERNAL).unwrap().contains("attribution"));
        // No derivatives: a trained model is one, internal or not.
        let nd = Dataset { licence: "CC-BY-NC-ND-4.0".into(), ..ok() };
        assert!(problem(&nd, INTERNAL).is_some());
        // ChessQueries and SLCC stay refused even for internal models.
        let cq = Dataset { name: "chessqueries".into(), ..ok() };
        assert!(problem(&cq, INTERNAL).is_some());
    }

    #[test]
    fn the_manifest_in_the_repo_loads_and_gates_as_expected() {
        let m = load_manifest(&Path::new(env!("CARGO_MANIFEST_DIR")).join("datasets.toml")).unwrap();
        assert_eq!(m["open-chess-photos"].format, "jsonl");
        assert_eq!(problem(&m["open-chess-photos"], SHARED), None);
        assert_eq!(problem(&m["open-chess-public"], SHARED), None);
        // ChessReD: commercial use by its author's permission, credited
        // (docs/dataset-permissions.md), so any model may use it.
        assert_eq!(problem(&m["chessred"], SHARED), None);
        assert!(!needs_internal(&m["chessred"]));
        assert!(m["chessred"].attribution.contains("End-to-End Chess Recognition"));
    }
}
