//! Independently versioned release components. Validation performs no I/O.
//!
//! Structural validation does not authenticate a publisher. Callers must verify
//! release provenance before using `validate_inventory_provenance`, and verify
//! downloaded artifact and primary-image hashes before executing anything.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub const INVENTORY_SCHEMA_VERSION: u32 = 1;
pub const MAX_INVENTORY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_PACKAGES: usize = 16;
const TARGETS: [&str; 8] = [
    "linux-x64",
    "linux-arm64",
    "windows-x64",
    "windows-arm64",
    "macos-x64",
    "macos-arm64",
    "android-arm64",
    "ios-arm64",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentId {
    App,
    Daemon,
    Tailnet,
    UpdateHelper,
}

impl ComponentId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Daemon => "daemon",
            Self::Tailnet => "tailnet",
            Self::UpdateHelper => "update-helper",
        }
    }

    pub const fn initial_abi_version(self) -> u32 {
        match self {
            Self::UpdateHelper => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for ComponentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ComponentId {
    type Err = ComponentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "app" => Ok(Self::App),
            "daemon" => Ok(Self::Daemon),
            "tailnet" => Ok(Self::Tailnet),
            "update-helper" => Ok(Self::UpdateHelper),
            _ => Err(ComponentError::Invalid("component id")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentIdentity {
    pub version: String,
    pub source_sha: String,
    pub input_hash: String,
    pub abi_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactRef {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub format: String,
    pub origin_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentRef {
    pub id: ComponentId,
    pub target: String,
    pub identity: ComponentIdentity,
    pub artifact: ArtifactRef,
    pub executable: String,
    pub executable_sha256: String,
    #[serde(deserialize_with = "deserialize_unique_map")]
    pub requires: BTreeMap<ComponentId, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageRef {
    pub target: String,
    pub version: String,
    pub source_sha: String,
    pub artifact: ArtifactRef,
    #[serde(deserialize_with = "deserialize_unique_map")]
    pub components: BTreeMap<ComponentId, ComponentIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseInventory {
    pub schema_version: u32,
    pub version: String,
    pub source_sha: String,
    pub channel: String,
    #[serde(deserialize_with = "deserialize_unique_map")]
    pub targets: BTreeMap<String, TargetInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetInventory {
    #[serde(deserialize_with = "deserialize_unique_map")]
    pub components: BTreeMap<ComponentId, ComponentRef>,
    pub packages: Vec<PackageRef>,
    pub atomic_groups: Vec<Vec<ComponentId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestartGroup {
    None,
    Daemon,
    Gui,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentUpdatePlan {
    pub changed: BTreeSet<ComponentId>,
    pub components: BTreeMap<ComponentId, ComponentRef>,
    pub restart_group: RestartGroup,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComponentError {
    #[error("component inventory exceeds size limit")]
    TooLarge,
    #[error("invalid component inventory JSON")]
    Json,
    #[error("invalid component field: {0}")]
    Invalid(&'static str),
    #[error("component {component} requires {dependency} ABI {required}")]
    Dependency {
        component: ComponentId,
        dependency: ComponentId,
        required: u32,
    },
    #[error("selected components do not include an entire changed atomic group")]
    AtomicGroup,
    #[error("component selection includes an unchanged or unavailable component")]
    Selection,
    #[error("release removes an installed component")]
    Removal,
    #[error("mobile component changes require an operating-system package update")]
    PackageBoundary,
    #[error("component or package lacks an exact authenticated origin reference")]
    Provenance,
}

pub type Result<T, E = ComponentError> = std::result::Result<T, E>;

fn deserialize_unique_map<'de, D, K, V>(
    deserializer: D,
) -> std::result::Result<BTreeMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    struct UniqueMap<K, V>(std::marker::PhantomData<(K, V)>);
    impl<'de, K, V> serde::de::Visitor<'de> for UniqueMap<K, V>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
    {
        type Value = BTreeMap<K, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded map without duplicate keys")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some(key) = access.next_key()? {
                if values.len() >= TARGETS.len() || values.contains_key(&key) {
                    return Err(serde::de::Error::custom(
                        "duplicate key or oversized component map",
                    ));
                }
                values.insert(key, access.next_value()?);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(UniqueMap(std::marker::PhantomData))
}

pub fn validate_version(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 || !value.is_ascii() {
        return Err(ComponentError::Invalid("version"));
    }
    let (without_build, build) = value
        .split_once('+')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    let (core, pre) = without_build
        .split_once('-')
        .map_or((without_build, None), |(a, b)| (a, Some(b)));
    let mut parts = core.split('.');
    for _ in 0..3 {
        let part = parts.next().ok_or(ComponentError::Invalid("version"))?;
        if !numeric_identifier(part) {
            return Err(ComponentError::Invalid("version"));
        }
    }
    if parts.next().is_some() {
        return Err(ComponentError::Invalid("version"));
    }
    for (identifiers, prerelease) in [(pre, true), (build, false)] {
        if let Some(identifiers) = identifiers {
            for part in identifiers.split('.') {
                if part.is_empty()
                    || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    || (prerelease
                        && part.bytes().all(|b| b.is_ascii_digit())
                        && !numeric_identifier(part))
                {
                    return Err(ComponentError::Invalid("version"));
                }
            }
        }
    }
    Ok(())
}

fn numeric_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value.len() == 1 || !value.starts_with('0'))
}

fn validate_hex(value: &str, len: usize, field: &'static str) -> Result<()> {
    if value.len() != len
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ComponentError::Invalid(field));
    }
    Ok(())
}

pub fn validate_sha256(value: &str) -> Result<()> {
    validate_hex(value, 64, "sha256")
}

pub fn validate_source_sha(value: &str) -> Result<()> {
    validate_hex(value, 40, "sourceSha")
}

pub fn validate_identity(identity: &ComponentIdentity) -> Result<()> {
    validate_version(&identity.version)?;
    validate_source_sha(&identity.source_sha)?;
    validate_sha256(&identity.input_hash)?;
    if identity.abi_version == 0 {
        return Err(ComponentError::Invalid("abiVersion"));
    }
    Ok(())
}

pub fn validate_repository(repository: &str) -> Result<()> {
    let Some((owner, repo)) = repository.split_once('/') else {
        return Err(ComponentError::Invalid("repository"));
    };
    if owner.is_empty()
        || owner.len() > 39
        || repo.is_empty()
        || repo.len() > 100
        || !owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || owner.starts_with('-')
        || owner.ends_with('-')
        || !repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || matches!(repo, "." | "..")
    {
        return Err(ComponentError::Invalid("repository"));
    }
    Ok(())
}

pub fn validate_target(target: &str) -> Result<()> {
    if !TARGETS.contains(&target) {
        return Err(ComponentError::Invalid("target"));
    }
    Ok(())
}

/// Platform-neutral path checks: reject Windows aliases even on Unix.
/// This validates syntax only; callers must additionally prevent symlink traversal.
pub fn validate_relative_image(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 1024 {
        return Err(ComponentError::Invalid("executable"));
    }
    for segment in path.split('/') {
        if segment.is_empty()
            || segment.len() > 255
            || matches!(segment, "." | "..")
            || segment.ends_with(['.', ' '])
            || segment.chars().any(|c| {
                c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '%')
            })
        {
            return Err(ComponentError::Invalid("executable"));
        }
        let stem = segment.split('.').next().unwrap_or("");
        if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
            .iter()
            .any(|name| stem.eq_ignore_ascii_case(name))
            || (stem.get(..3).is_some_and(|prefix| {
                prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
            }) && ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"]
                .contains(&stem.get(3..).unwrap_or("")))
        {
            return Err(ComponentError::Invalid("executable"));
        }
    }
    Ok(())
}

fn tag_version(tag: &str) -> Result<&str> {
    for prefix in [
        "desktop-windows-v",
        "desktop-linux-v",
        "desktop-macos-v",
        "desktop-v",
        "android-v",
        "app-v",
        "v",
    ] {
        if let Some(version) = tag.strip_prefix(prefix) {
            validate_version(version)?;
            return Ok(version);
        }
    }
    Err(ComponentError::Invalid("originTag"))
}

pub fn validate_artifact(artifact: &ArtifactRef, repository: &str, version: &str) -> Result<()> {
    validate_repository(repository)?;
    validate_sha256(&artifact.sha256)?;
    if artifact.size == 0 || artifact.size > MAX_ARTIFACT_BYTES {
        return Err(ComponentError::Invalid("artifact size"));
    }
    if ![
        "tar.gz", "zip", "AppImage", "appimage", "deb", "exe", "dmg", "apk", "ipa",
    ]
    .contains(&artifact.format.as_str())
    {
        return Err(ComponentError::Invalid("artifact format"));
    }
    if tag_version(&artifact.origin_tag)? != version || artifact.url.len() > 2048 {
        return Err(ComponentError::Invalid("originTag"));
    }
    // Check the original spelling, not URL-normalized paths (which erase ../).
    let prefix = format!(
        "https://github.com/{repository}/releases/download/{}/",
        artifact.origin_tag
    );
    let asset = artifact
        .url
        .strip_prefix(&prefix)
        .ok_or(ComponentError::Invalid("artifact url"))?;
    if asset.is_empty()
        || asset.len() > 255
        || matches!(asset, "." | "..")
        || !asset
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
    {
        return Err(ComponentError::Invalid("artifact url"));
    }
    Ok(())
}

/// Syntactic component validation. Delivery provenance is target-relative;
/// callers must also validate the enclosing target before trusting this ref.
pub fn validate_component(component: &ComponentRef, target: &str, repository: &str) -> Result<()> {
    validate_target(target)?;
    if component.target != target {
        return Err(ComponentError::Invalid("component target"));
    }
    validate_identity(&component.identity)?;
    validate_artifact(
        &component.artifact,
        repository,
        tag_version(&component.artifact.origin_tag)?,
    )?;
    validate_relative_image(&component.executable)?;
    validate_sha256(&component.executable_sha256)?;
    if component.requires.contains_key(&component.id)
        || component.requires.values().any(|abi| *abi == 0)
    {
        return Err(ComponentError::Invalid("requires"));
    }
    if component.id == ComponentId::App {
        let mobile = target.starts_with("android-") || target.starts_with("ios-");
        for dependency in [
            ComponentId::Tailnet,
            ComponentId::Daemon,
            ComponentId::UpdateHelper,
        ] {
            if (!mobile || dependency == ComponentId::Tailnet)
                && !component.requires.contains_key(&dependency)
            {
                return Err(ComponentError::Invalid("app ABI requirements"));
            }
        }
    }
    Ok(())
}

pub fn required_atomic_groups(target: &str) -> Result<Vec<Vec<ComponentId>>> {
    validate_target(target)?;
    if target.starts_with("macos-") || target.starts_with("ios-") || target.starts_with("android-")
    {
        Ok(vec![vec![ComponentId::App, ComponentId::Tailnet]])
    } else {
        Ok(Vec::new())
    }
}

/// Authenticated delivery identity, which may differ from the compiled identity
/// only for an explicitly described mandatory signed operating-system cohort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactOrigin<'a> {
    pub version: &'a str,
    pub source_sha: &'a str,
    pub artifact: &'a ArtifactRef,
    pub package: Option<&'a PackageRef>,
}

/// Derive delivery provenance without guessing from a binary's version. This
/// helper checks the exception's complete mapping; callers must separately
/// validate the target and authenticate the referenced release publisher.
pub fn component_artifact_origin(
    inventory: &TargetInventory,
    id: ComponentId,
) -> Result<ArtifactOrigin<'_>> {
    let component = inventory
        .components
        .get(&id)
        .ok_or(ComponentError::Invalid("component id"))?;
    let mut matched_package = None;
    for required in required_atomic_groups(&component.target)? {
        if !required.contains(&id) {
            continue;
        }
        let Some(group) = inventory
            .atomic_groups
            .iter()
            .find(|group| required.iter().all(|member| group.contains(member)))
        else {
            continue;
        };
        for package in &inventory.packages {
            if package.target != component.target || package.artifact != component.artifact {
                continue;
            }
            if !group.iter().all(|member| {
                inventory.components.get(member).is_some_and(|entry| {
                    entry.target == component.target
                        && entry.artifact == component.artifact
                        && package.components.get(member) == Some(&entry.identity)
                })
            }) {
                return Err(ComponentError::Invalid("signed cohort package identities"));
            }
            if matched_package.replace(package).is_some() {
                return Err(ComponentError::Invalid("ambiguous signed cohort package"));
            }
        }
    }
    if let Some(package) = matched_package {
        validate_version(&package.version)?;
        validate_source_sha(&package.source_sha)?;
        if tag_version(&component.artifact.origin_tag)? != package.version {
            return Err(ComponentError::Invalid("signed cohort originTag"));
        }
        return Ok(ArtifactOrigin {
            version: &package.version,
            source_sha: &package.source_sha,
            artifact: &component.artifact,
            package: Some(package),
        });
    }
    if tag_version(&component.artifact.origin_tag)? != component.identity.version {
        return Err(ComponentError::Invalid("component originTag"));
    }
    Ok(ArtifactOrigin {
        version: &component.identity.version,
        source_sha: &component.identity.source_sha,
        artifact: &component.artifact,
        package: None,
    })
}

fn validate_dependencies(components: &BTreeMap<ComponentId, ComponentRef>) -> Result<()> {
    for (id, component) in components {
        for (dependency, abi) in &component.requires {
            if components
                .get(dependency)
                .is_none_or(|entry| entry.identity.abi_version != *abi)
            {
                return Err(ComponentError::Dependency {
                    component: *id,
                    dependency: *dependency,
                    required: *abi,
                });
            }
        }
    }
    Ok(())
}

pub fn validate_target_inventory(
    inventory: &TargetInventory,
    target: &str,
    repository: &str,
) -> Result<()> {
    validate_repository(repository)?;
    validate_target(target)?;
    if inventory.components.is_empty()
        || inventory.components.len() > 4
        || inventory.packages.len() > MAX_PACKAGES
        || inventory.atomic_groups.len() > 2
    {
        return Err(ComponentError::Invalid("target inventory bounds"));
    }
    for (id, component) in &inventory.components {
        if *id != component.id {
            return Err(ComponentError::Invalid("component map key"));
        }
        validate_component(component, target, repository)?;
    }
    validate_dependencies(&inventory.components)?;
    let mut grouped = BTreeSet::new();
    for group in &inventory.atomic_groups {
        if group.len() < 2 || group.len() > 4 {
            return Err(ComponentError::Invalid("atomicGroups"));
        }
        for id in group {
            if !inventory.components.contains_key(id) || !grouped.insert(*id) {
                return Err(ComponentError::Invalid("atomicGroups"));
            }
        }
    }
    for required in required_atomic_groups(target)? {
        if required
            .iter()
            .any(|id| inventory.components.contains_key(id))
            && !inventory
                .atomic_groups
                .iter()
                .any(|group| required.iter().all(|id| group.contains(id)))
        {
            return Err(ComponentError::Invalid("target atomic group"));
        }
    }
    if target.starts_with("macos-")
        && let Some(app) = inventory.components.get(&ComponentId::App)
    {
        let tailnet = inventory
            .components
            .get(&ComponentId::Tailnet)
            .ok_or(ComponentError::AtomicGroup)?;
        if app.artifact != tailnet.artifact {
            return Err(ComponentError::Invalid("signed app cohort artifact"));
        }
    }
    let mut package_urls = BTreeSet::new();
    for package in &inventory.packages {
        if package.target != target
            || package.components.is_empty()
            || !package_urls.insert(&package.artifact.url)
        {
            return Err(ComponentError::Invalid("package"));
        }
        validate_version(&package.version)?;
        validate_source_sha(&package.source_sha)?;
        validate_artifact(&package.artifact, repository, &package.version)?;
        for (id, identity) in &package.components {
            validate_identity(identity)?;
            if !inventory.components.contains_key(id) {
                return Err(ComponentError::Invalid("package component id"));
            }
        }
        for group in &inventory.atomic_groups {
            if group.iter().any(|id| package.components.contains_key(id))
                && !group.iter().all(|id| package.components.contains_key(id))
            {
                return Err(ComponentError::Invalid("package atomic group"));
            }
        }
    }
    for id in inventory.components.keys() {
        component_artifact_origin(inventory, *id)?;
    }
    Ok(())
}

pub fn validate_inventory(inventory: &ReleaseInventory, repository: &str) -> Result<()> {
    validate_repository(repository)?;
    if inventory.schema_version != INVENTORY_SCHEMA_VERSION
        || inventory.targets.is_empty()
        || inventory.targets.len() > TARGETS.len()
    {
        return Err(ComponentError::Invalid("inventory bounds/schemaVersion"));
    }
    validate_version(&inventory.version)?;
    validate_source_sha(&inventory.source_sha)?;
    let prerelease = inventory
        .version
        .split('+')
        .next()
        .unwrap_or("")
        .contains('-');
    if inventory.channel != if prerelease { "beta" } else { "stable" } {
        return Err(ComponentError::Invalid("channel"));
    }
    for (target, entries) in &inventory.targets {
        validate_target_inventory(entries, target, repository)?;
        for component in entries.components.values() {
            let origin = component_artifact_origin(entries, component.id)?;
            if origin.version == inventory.version && origin.source_sha != inventory.source_sha {
                return Err(ComponentError::Invalid("current artifact sourceSha"));
            }
            if component.identity.version == inventory.version
                && component.identity.source_sha != inventory.source_sha
            {
                return Err(ComponentError::Invalid("current component sourceSha"));
            }
        }
        for package in &entries.packages {
            if package.version == inventory.version && package.source_sha != inventory.source_sha {
                return Err(ComponentError::Invalid("current package sourceSha"));
            }
        }
    }
    Ok(())
}

/// Authenticated origins must be fetched and publisher/provenance-verified by
/// the caller. A structurally valid inventory alone is never a trust anchor.
pub fn validate_inventory_provenance(
    inventory: &ReleaseInventory,
    verified_origins: &BTreeMap<String, ReleaseInventory>,
    repository: &str,
) -> Result<()> {
    validate_inventory(inventory, repository)?;
    for (target, entries) in &inventory.targets {
        for component in entries.components.values() {
            let delivery = component_artifact_origin(entries, component.id)?;
            if delivery.version == inventory.version {
                continue;
            }
            let origin = verified_origins
                .get(&component.artifact.origin_tag)
                .ok_or(ComponentError::Provenance)?;
            validate_inventory(origin, repository)?;
            if origin.version != delivery.version
                || origin.source_sha != delivery.source_sha
                || origin
                    .targets
                    .get(target)
                    .and_then(|entries| entries.components.get(&component.id))
                    != Some(component)
            {
                return Err(ComponentError::Provenance);
            }
        }
        for package in &entries.packages {
            if package.version == inventory.version {
                continue;
            }
            let origin = verified_origins
                .get(&package.artifact.origin_tag)
                .ok_or(ComponentError::Provenance)?;
            validate_inventory(origin, repository)?;
            if origin.version != package.version
                || origin.source_sha != package.source_sha
                || !origin
                    .targets
                    .get(target)
                    .is_some_and(|entries| entries.packages.contains(package))
            {
                return Err(ComponentError::Provenance);
            }
        }
    }
    Ok(())
}

pub fn parse_inventory(bytes: &[u8], repository: &str) -> Result<ReleaseInventory> {
    if bytes.len() > MAX_INVENTORY_BYTES {
        return Err(ComponentError::TooLarge);
    }
    let inventory: ReleaseInventory =
        serde_json::from_slice(bytes).map_err(|_| ComponentError::Json)?;
    validate_inventory(&inventory, repository)?;
    Ok(inventory)
}

pub fn changed_components(
    current: &TargetInventory,
    available: &TargetInventory,
) -> BTreeSet<ComponentId> {
    available
        .components
        .iter()
        .filter_map(|(id, entry)| (current.components.get(id) != Some(entry)).then_some(*id))
        .collect()
}

/// Select only explicitly requested changes, or all available changes. The
/// resulting graph, not equality of product versions, determines compatibility.
pub fn plan_updates(
    target: &str,
    repository: &str,
    current: &TargetInventory,
    available: &TargetInventory,
    selected: Option<&BTreeSet<ComponentId>>,
) -> Result<ComponentUpdatePlan> {
    validate_target_inventory(current, target, repository)?;
    validate_target_inventory(available, target, repository)?;
    if current
        .components
        .keys()
        .any(|id| !available.components.contains_key(id))
    {
        return Err(ComponentError::Removal);
    }
    let changes = changed_components(current, available);
    let selected = selected.unwrap_or(&changes);
    if !selected.is_subset(&changes) {
        return Err(ComponentError::Selection);
    }
    for group in current.atomic_groups.iter().chain(&available.atomic_groups) {
        if group.iter().any(|id| selected.contains(id))
            && group
                .iter()
                .any(|id| changes.contains(id) && !selected.contains(id))
        {
            return Err(ComponentError::AtomicGroup);
        }
    }
    if !selected.is_empty() && (target.starts_with("ios-") || target.starts_with("android-")) {
        return Err(ComponentError::PackageBoundary);
    }
    let mut components = current.components.clone();
    for id in selected {
        components.insert(*id, available.components[id].clone());
    }
    validate_dependencies(&components)?;
    let restart_group =
        if selected.contains(&ComponentId::App) || selected.contains(&ComponentId::Tailnet) {
            RestartGroup::Gui
        } else if selected.contains(&ComponentId::Daemon) {
            RestartGroup::Daemon
        } else {
            RestartGroup::None
        };
    Ok(ComponentUpdatePlan {
        changed: selected.clone(),
        components,
        restart_group,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "emotionbug/Passession";
    const TARGET: &str = "linux-x64";

    fn component(id: ComponentId, version: &str) -> ComponentRef {
        ComponentRef {
            id,
            target: TARGET.into(),
            identity: ComponentIdentity {
                version: version.into(),
                source_sha: "a".repeat(40),
                input_hash: "b".repeat(64),
                abi_version: id.initial_abi_version(),
            },
            artifact: ArtifactRef {
                url: format!(
                    "https://github.com/{REPO}/releases/download/v{version}/{id}-{TARGET}.tar.gz"
                ),
                sha256: "c".repeat(64),
                size: 123,
                format: "tar.gz".into(),
                origin_tag: format!("v{version}"),
            },
            executable: format!("bin/{id}"),
            executable_sha256: "d".repeat(64),
            requires: BTreeMap::new(),
        }
    }

    fn target() -> TargetInventory {
        let mut app = component(ComponentId::App, "0.13.25");
        app.requires.insert(ComponentId::Daemon, 1);
        app.requires.insert(ComponentId::Tailnet, 1);
        app.requires.insert(ComponentId::UpdateHelper, 2);
        TargetInventory {
            components: [
                app,
                component(ComponentId::Daemon, "0.13.25"),
                component(ComponentId::Tailnet, "0.13.25"),
                component(ComponentId::UpdateHelper, "0.13.25"),
            ]
            .into_iter()
            .map(|entry| (entry.id, entry))
            .collect(),
            packages: Vec::new(),
            atomic_groups: Vec::new(),
        }
    }

    fn inventory() -> ReleaseInventory {
        ReleaseInventory {
            schema_version: 1,
            version: "0.13.25".into(),
            source_sha: "a".repeat(40),
            channel: "stable".into(),
            targets: [(TARGET.into(), target())].into(),
        }
    }

    #[test]
    fn roundtrip_and_no_change_do_not_align_collection_versions() {
        let old = inventory();
        let mut next = old.clone();
        next.version = "0.13.26".into();
        next.source_sha = "e".repeat(40);
        let bytes = serde_json::to_vec(&next).unwrap();
        assert_eq!(parse_inventory(&bytes, REPO).unwrap(), next);
        let plan = plan_updates(
            TARGET,
            REPO,
            &old.targets[TARGET],
            &next.targets[TARGET],
            None,
        )
        .unwrap();
        assert!(plan.changed.is_empty());
        assert_eq!(plan.restart_group, RestartGroup::None);
        assert_eq!(
            plan.components[&ComponentId::App].identity.version,
            "0.13.25"
        );
    }

    #[test]
    fn malformed_inventory_and_security_fields_are_rejected() {
        let base = inventory();
        let mut json = serde_json::to_value(&base).unwrap();
        json["targets"][TARGET]["components"]["app"]["trusted"] = true.into();
        assert_eq!(
            parse_inventory(&serde_json::to_vec(&json).unwrap(), REPO),
            Err(ComponentError::Json)
        );
        assert_eq!(
            parse_inventory(&vec![b' '; MAX_INVENTORY_BYTES + 1], REPO),
            Err(ComponentError::TooLarge)
        );
        for version in [
            "1.2",
            "01.2.3",
            "1.2.3-01",
            "1.2.3+",
            "1.2.3-a..b",
            "1.2.3+evil?x",
        ] {
            assert!(validate_version(version).is_err(), "{version}");
        }
        for path in [
            "/bin/app",
            "../app",
            "bin//app",
            "C:/app",
            "bin\\app",
            "bin/%2e%2e/app",
            "bin/CON.exe",
            "bin/app.",
            "bin/app:stream",
        ] {
            assert!(validate_relative_image(path).is_err(), "{path}");
        }
        assert!(validate_relative_image("Paseo.app/Contents/MacOS/Paseo").is_ok());
        for url in [
            "http://github.com/emotionbug/Passession/releases/download/v0.13.25/a.tar.gz",
            "https://github.com@evil.test/emotionbug/Passession/releases/download/v0.13.25/a.tar.gz",
            "https://github.com/other/repo/releases/download/v0.13.25/a.tar.gz",
            "https://github.com/emotionbug/Passession/releases/download/v0.13.25/a.tar.gz?token=x",
            "https://github.com/emotionbug/Passession/releases/download/v0.13.25/../a.tar.gz",
        ] {
            let mut entry = component(ComponentId::Daemon, "0.13.25");
            entry.artifact.url = url.into();
            assert!(validate_component(&entry, TARGET, REPO).is_err());
        }
    }

    #[test]
    fn only_changed_compatible_components_are_selected() {
        let current = target();
        let mut available = current.clone();
        available.components.insert(
            ComponentId::Daemon,
            component(ComponentId::Daemon, "0.13.26"),
        );
        let plan = plan_updates(TARGET, REPO, &current, &available, None).unwrap();
        assert_eq!(plan.changed, [ComponentId::Daemon].into());
        assert_eq!(plan.restart_group, RestartGroup::Daemon);
        assert_eq!(
            plan.components[&ComponentId::App],
            current.components[&ComponentId::App]
        );
        available.components.insert(
            ComponentId::UpdateHelper,
            component(ComponentId::UpdateHelper, "0.13.26"),
        );
        let selected = [ComponentId::UpdateHelper].into();
        let plan = plan_updates(TARGET, REPO, &current, &available, Some(&selected)).unwrap();
        assert_eq!(plan.restart_group, RestartGroup::None);
        assert_eq!(
            plan.components[&ComponentId::Daemon],
            current.components[&ComponentId::Daemon]
        );
        assert!(
            plan_updates(
                TARGET,
                REPO,
                &current,
                &available,
                Some(&[ComponentId::App].into())
            )
            .is_err()
        );
    }

    #[test]
    fn dependency_upgrades_require_compatible_consumers() {
        let current = target();
        let mut available = current.clone();
        let mut daemon = component(ComponentId::Daemon, "0.13.26");
        daemon.identity.abi_version = 2;
        available.components.insert(ComponentId::Daemon, daemon);
        assert!(validate_target_inventory(&available, TARGET, REPO).is_err());
        let mut app = component(ComponentId::App, "0.13.26");
        app.requires.insert(ComponentId::Daemon, 2);
        app.requires.insert(ComponentId::Tailnet, 1);
        app.requires.insert(ComponentId::UpdateHelper, 2);
        available.components.insert(ComponentId::App, app);
        assert!(plan_updates(TARGET, REPO, &current, &available, None).is_ok());
        assert!(matches!(
            plan_updates(
                TARGET,
                REPO,
                &current,
                &available,
                Some(&[ComponentId::Daemon].into())
            ),
            Err(ComponentError::Dependency { .. })
        ));
        available
            .components
            .get_mut(&ComponentId::App)
            .unwrap()
            .requires
            .insert(ComponentId::Tailnet, 77);
        assert!(validate_target_inventory(&available, TARGET, REPO).is_err());
    }

    #[test]
    fn mixed_sources_require_exact_authenticated_original_refs() {
        let old = inventory();
        let mut next = old.clone();
        next.version = "0.13.26".into();
        next.source_sha = "e".repeat(40);
        let mut daemon = component(ComponentId::Daemon, "0.13.26");
        daemon.identity.source_sha = next.source_sha.clone();
        next.targets
            .get_mut(TARGET)
            .unwrap()
            .components
            .insert(ComponentId::Daemon, daemon);
        assert_eq!(
            validate_inventory_provenance(&next, &BTreeMap::new(), REPO),
            Err(ComponentError::Provenance)
        );
        let origins = [("v0.13.25".into(), old)].into();
        assert!(validate_inventory_provenance(&next, &origins, REPO).is_ok());
        next.targets
            .get_mut(TARGET)
            .unwrap()
            .components
            .get_mut(&ComponentId::App)
            .unwrap()
            .identity
            .input_hash = "f".repeat(64);
        assert_eq!(
            validate_inventory_provenance(&next, &origins, REPO),
            Err(ComponentError::Provenance)
        );
    }

    #[test]
    fn atomic_groups_and_mobile_package_boundaries_are_enforced() {
        let mut current = target();
        current.atomic_groups = vec![vec![ComponentId::App, ComponentId::Tailnet]];
        let mut available = current.clone();
        for id in [ComponentId::App, ComponentId::Tailnet] {
            let mut replacement = component(id, "0.13.26");
            replacement.requires = current.components[&id].requires.clone();
            available.components.insert(id, replacement);
        }
        assert_eq!(
            plan_updates(
                TARGET,
                REPO,
                &current,
                &available,
                Some(&[ComponentId::App].into())
            ),
            Err(ComponentError::AtomicGroup)
        );
        assert_eq!(
            plan_updates(TARGET, REPO, &current, &available, None)
                .unwrap()
                .restart_group,
            RestartGroup::Gui
        );
        for inventory in [&mut current, &mut available] {
            for entry in inventory.components.values_mut() {
                entry.target = "android-arm64".into();
            }
        }
        assert_eq!(
            plan_updates("android-arm64", REPO, &current, &available, None),
            Err(ComponentError::PackageBoundary)
        );
        available.atomic_groups.clear();
        assert!(validate_target_inventory(&available, "android-arm64", REPO).is_err());
    }

    #[test]
    fn mismatched_maps_packages_and_missing_dependencies_fail_closed() {
        let mut entries = target();
        entries.components.get_mut(&ComponentId::Daemon).unwrap().id = ComponentId::App;
        assert!(validate_target_inventory(&entries, TARGET, REPO).is_err());
        let mut entries = target();
        entries.components.remove(&ComponentId::Tailnet);
        assert!(validate_target_inventory(&entries, TARGET, REPO).is_err());
        let mut entries = target();
        let app = &entries.components[&ComponentId::App];
        entries.packages.push(PackageRef {
            target: TARGET.into(),
            version: "0.13.25".into(),
            source_sha: "a".repeat(40),
            artifact: app.artifact.clone(),
            components: [(ComponentId::App, app.identity.clone())].into(),
        });
        entries.packages[0]
            .components
            .get_mut(&ComponentId::App)
            .unwrap()
            .source_sha = "e".repeat(39);
        assert!(validate_target_inventory(&entries, TARGET, REPO).is_err());
    }

    #[test]
    fn duplicate_component_keys_are_rejected_before_validation() {
        let serialized = serde_json::to_string(&component(ComponentId::Daemon, "0.13.25")).unwrap();
        let duplicate = format!(
            "{{\"components\":{{\"daemon\":{serialized},\"daemon\":{serialized}}},\"packages\":[],\"atomicGroups\":[]}}"
        );
        assert!(serde_json::from_str::<TargetInventory>(&duplicate).is_err());
    }

    #[test]
    fn signed_macos_cohort_cannot_reference_separate_archives() {
        let mut entries = target();
        for component in entries.components.values_mut() {
            component.target = "macos-arm64".into();
        }
        entries.atomic_groups = required_atomic_groups("macos-arm64").unwrap();
        assert!(validate_target_inventory(&entries, "macos-arm64", REPO).is_err());
        entries
            .components
            .get_mut(&ComponentId::Tailnet)
            .unwrap()
            .artifact = entries.components[&ComponentId::App].artifact.clone();
        assert!(validate_target_inventory(&entries, "macos-arm64", REPO).is_ok());
    }

    #[test]
    fn identities_targets_and_dependency_declarations_are_not_optional() {
        let base = component(ComponentId::Daemon, "0.13.25");
        let mut bad = base.clone();
        bad.identity.source_sha = "a".repeat(39);
        assert!(validate_component(&bad, TARGET, REPO).is_err());
        bad = base.clone();
        bad.identity.input_hash = "G".repeat(64);
        assert!(validate_component(&bad, TARGET, REPO).is_err());
        bad = base.clone();
        bad.identity.abi_version = 0;
        assert!(validate_component(&bad, TARGET, REPO).is_err());
        bad = base.clone();
        bad.artifact.origin_tag = "v0.13.26".into();
        assert!(validate_component(&bad, TARGET, REPO).is_err());
        bad = base;
        bad.target = "windows-x64".into();
        assert!(validate_component(&bad, TARGET, REPO).is_err());
        let mut entries = target();
        entries
            .components
            .get_mut(&ComponentId::App)
            .unwrap()
            .requires
            .clear();
        assert!(validate_target_inventory(&entries, TARGET, REPO).is_err());
        assert!(validate_relative_image("bin/COM¹.dll").is_err());
        assert!(validate_relative_image("bin/éé").is_ok());
    }

    #[test]
    fn reused_artifact_or_primary_image_mutation_fails_origin_validation() {
        let old = inventory();
        let mut next = old.clone();
        next.version = "0.13.26".into();
        next.source_sha = "e".repeat(40);
        let origins = [("v0.13.25".into(), old)].into();
        let mut bad = next.clone();
        bad.targets
            .get_mut(TARGET)
            .unwrap()
            .components
            .get_mut(&ComponentId::Daemon)
            .unwrap()
            .artifact
            .sha256 = "f".repeat(64);
        assert_eq!(
            validate_inventory_provenance(&bad, &origins, REPO),
            Err(ComponentError::Provenance)
        );
        next.targets
            .get_mut(TARGET)
            .unwrap()
            .components
            .get_mut(&ComponentId::Daemon)
            .unwrap()
            .executable_sha256 = "f".repeat(64);
        assert_eq!(
            validate_inventory_provenance(&next, &origins, REPO),
            Err(ComponentError::Provenance)
        );
    }

    #[test]
    fn signed_repack_preserves_compiled_identities_and_pins_delivery_source() {
        const MAC: &str = "macos-arm64";
        let mut original_target = target();
        for entry in original_target.components.values_mut() {
            entry.target = MAC.into();
        }
        original_target.atomic_groups = required_atomic_groups(MAC).unwrap();
        original_target
            .components
            .get_mut(&ComponentId::Tailnet)
            .unwrap()
            .artifact = original_target.components[&ComponentId::App]
            .artifact
            .clone();
        let old = ReleaseInventory {
            schema_version: 1,
            version: "0.13.25".into(),
            source_sha: "a".repeat(40),
            channel: "stable".into(),
            targets: [(MAC.into(), original_target.clone())].into(),
        };
        let mut repack = old.clone();
        repack.version = "0.13.26".into();
        repack.source_sha = "e".repeat(40);
        let artifact = ArtifactRef {
            url: format!(
                "https://github.com/{REPO}/releases/download/v0.13.26/signed-macos-arm64.zip"
            ),
            sha256: "e".repeat(64),
            size: 234,
            format: "zip".into(),
            origin_tag: "v0.13.26".into(),
        };
        let entries = repack.targets.get_mut(MAC).unwrap();
        for id in [ComponentId::App, ComponentId::Tailnet] {
            let entry = entries.components.get_mut(&id).unwrap();
            entry.artifact = artifact.clone();
            entry.executable_sha256 = "e".repeat(64);
        }
        // A component parser cannot decide whether its signed container is legitimate.
        assert!(validate_component(&entries.components[&ComponentId::Tailnet], MAC, REPO).is_ok());
        assert!(validate_target_inventory(entries, MAC, REPO).is_err());
        entries.packages.push(PackageRef {
            target: MAC.into(),
            version: "0.13.26".into(),
            source_sha: "e".repeat(40),
            artifact,
            components: [ComponentId::App, ComponentId::Tailnet]
                .into_iter()
                .map(|id| (id, entries.components[&id].identity.clone()))
                .collect(),
        });
        let origin = component_artifact_origin(entries, ComponentId::Tailnet).unwrap();
        assert_eq!(origin.version, "0.13.26");
        assert_eq!(origin.source_sha, "e".repeat(40));
        assert!(origin.package.is_some());
        let plan = plan_updates(MAC, REPO, &original_target, entries, None).unwrap();
        assert_eq!(
            plan.changed,
            [ComponentId::App, ComponentId::Tailnet].into()
        );
        assert_eq!(plan.restart_group, RestartGroup::Gui);
        assert_eq!(
            plan.components[&ComponentId::Tailnet].identity,
            original_target.components[&ComponentId::Tailnet].identity
        );
        assert!(
            plan_updates(
                MAC,
                REPO,
                &original_target,
                entries,
                Some(&[ComponentId::App].into())
            )
            .is_err()
        );
        let mut origins = BTreeMap::from([("v0.13.25".into(), old)]);
        assert!(validate_inventory_provenance(&repack, &origins, REPO).is_ok());
        let mut next = repack.clone();
        next.version = "0.13.27".into();
        next.source_sha = "f".repeat(40);
        assert!(validate_inventory_provenance(&next, &origins, REPO).is_err());
        origins.insert("v0.13.26".into(), repack.clone());
        assert!(validate_inventory_provenance(&next, &origins, REPO).is_ok());
        let mut missing_member = repack.clone();
        missing_member.targets.get_mut(MAC).unwrap().packages[0]
            .components
            .remove(&ComponentId::Tailnet);
        assert!(validate_inventory(&missing_member, REPO).is_err());
        let mut wrong_identity = repack.clone();
        wrong_identity.targets.get_mut(MAC).unwrap().packages[0]
            .components
            .get_mut(&ComponentId::Tailnet)
            .unwrap()
            .input_hash = "f".repeat(64);
        assert!(validate_inventory(&wrong_identity, REPO).is_err());
        let mut wrong_source = repack.clone();
        wrong_source.targets.get_mut(MAC).unwrap().packages[0].source_sha = "f".repeat(40);
        assert!(validate_inventory(&wrong_source, REPO).is_err());
        repack.targets.get_mut(MAC).unwrap().atomic_groups.clear();
        assert!(validate_inventory(&repack, REPO).is_err());
    }

    #[test]
    fn daemon_only_update_keeps_authenticated_historical_bootstrap_package() {
        let mut old = inventory();
        let entries = old.targets.get_mut(TARGET).unwrap();
        let package = PackageRef {
            target: TARGET.into(),
            version: "0.13.25".into(),
            source_sha: old.source_sha.clone(),
            artifact: entries.components[&ComponentId::App].artifact.clone(),
            components: entries
                .components
                .iter()
                .map(|(id, entry)| (*id, entry.identity.clone()))
                .collect(),
        };
        entries.packages.push(package.clone());
        let mut next = old.clone();
        next.version = "0.13.26".into();
        next.source_sha = "e".repeat(40);
        let mut daemon = component(ComponentId::Daemon, "0.13.26");
        daemon.identity.source_sha = next.source_sha.clone();
        next.targets
            .get_mut(TARGET)
            .unwrap()
            .components
            .insert(ComponentId::Daemon, daemon);
        let plan = plan_updates(
            TARGET,
            REPO,
            &old.targets[TARGET],
            &next.targets[TARGET],
            None,
        )
        .unwrap();
        assert_eq!(plan.changed, [ComponentId::Daemon].into());
        assert_eq!(next.targets[TARGET].packages, vec![package]);
        let origins = BTreeMap::from([("v0.13.25".into(), old)]);
        assert!(validate_inventory_provenance(&next, &origins, REPO).is_ok());
        next.targets.get_mut(TARGET).unwrap().packages[0]
            .components
            .get_mut(&ComponentId::Daemon)
            .unwrap()
            .source_sha = "f".repeat(40);
        assert_eq!(
            validate_inventory_provenance(&next, &origins, REPO),
            Err(ComponentError::Provenance)
        );
    }
}
