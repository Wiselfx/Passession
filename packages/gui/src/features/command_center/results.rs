//! Command Center results: contribution sections ordered by match quality,
//! with pinned file/session sections and recency tie-breaks.

use std::cmp::Ordering;

use serde_json::Value;

use super::matching::{MatchScore, compare_scores, score_text_fields};
use crate::common::UiEffect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Agent,
    File,
    Action,
}

/// What a chosen row does once the palette has closed.
#[derive(Clone, Debug)]
pub enum Run {
    /// A shell command (`onCommand: _execute`).
    Command(String, Value),
    Effect(UiEffect),
    /// Toast an error instead of acting.
    Error(String),
}

#[derive(Clone, Debug)]
pub enum Icon {
    None,
    Glyph(&'static str),
    Provider {
        provider: String,
        bucket: Option<crate::common::widgets::StatusBucket>,
    },
    File(String),
}

/// 0 when every token matched visible text, 1 when one only matched a hidden keyword.
#[derive(Clone, Copy, Debug)]
pub struct Score {
    pub field_rank: i64,
    pub matched: MatchScore,
}

#[derive(Clone, Debug)]
pub struct ResultRow {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub subtitle: String,
    /// Matched but never rendered (an agent's cwd).
    pub hidden: Vec<String>,
    /// Activity time in ms since epoch.
    pub activity_at: Option<i64>,
    pub shortcut: String,
    pub icon: Icon,
    pub score: Option<Score>,
    pub run: Run,
}

impl ResultRow {
    pub fn new(id: impl Into<String>, kind: Kind, title: impl Into<String>, run: Run) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            subtitle: String::new(),
            hidden: Vec::new(),
            activity_at: None,
            shortcut: String::new(),
            icon: Icon::None,
            score: None,
            run,
        }
    }

    pub fn two_line(&self) -> bool {
        self.kind == Kind::Agent || (self.kind == Kind::Action && !self.subtitle.is_empty())
    }
}

pub fn score_fields(query: &str, visible: &[&str], hidden: &[String]) -> Option<Score> {
    if let Some(matched) = score_text_fields(query, visible, false) {
        return Some(Score {
            field_rank: 0,
            matched,
        });
    }
    if hidden.is_empty() {
        return None;
    }
    let mut all: Vec<&str> = visible.to_vec();
    all.extend(hidden.iter().map(String::as_str));
    score_text_fields(query, &all, false).map(|matched| Score {
        field_rank: 1,
        matched,
    })
}

fn compare(a: &Score, b: &Score) -> Ordering {
    a.field_rank
        .cmp(&b.field_rank)
        .then_with(|| compare_scores(&a.matched, &b.matched))
}

/// Built-in rows rank by which fields matched and the tier only; recency breaks ties.
fn compare_built_in(a: &Score, b: &Score) -> Ordering {
    a.field_rank
        .cmp(&b.field_rank)
        .then(a.matched.tier.cmp(&b.matched.tier))
}

/// Most recent first; rows without a time sort after every row with one.
fn by_recency(a: &ResultRow, b: &ResultRow) -> Ordering {
    b.activity_at.unwrap_or(0).cmp(&a.activity_at.unwrap_or(0))
}

/// `filterAndRankBuiltInResults`: the five most recent rows with no query, else every match
/// by score then recency; ties keep input order.
pub fn rank_built_in(rows: Vec<ResultRow>, query: &str) -> Vec<ResultRow> {
    let mut indexed: Vec<(usize, ResultRow)> = rows.into_iter().enumerate().collect();
    if query.trim().is_empty() {
        indexed.sort_by(|a, b| by_recency(&a.1, &b.1).then(a.0.cmp(&b.0)));
        return indexed.into_iter().take(5).map(|(_, r)| r).collect();
    }
    let mut scored: Vec<(usize, ResultRow, Score)> = indexed
        .into_iter()
        .filter_map(|(i, row)| {
            score_fields(query, &[&row.title, &row.subtitle], &row.hidden).map(|s| (i, row, s))
        })
        .collect();
    scored.sort_by(|a, b| {
        compare_built_in(&a.2, &b.2)
            .then_with(|| by_recency(&a.1, &b.1))
            .then(a.0.cmp(&b.0))
    });
    scored.into_iter().map(|(_, r, _)| r).collect()
}

/// A registered action (presentation kind "action").
#[derive(Clone, Debug)]
pub struct Contribution {
    pub id: String,
    pub group: String,
    pub group_rank: i64,
    pub rank: i64,
    pub keywords: Vec<String>,
    /// Listed only once something has been typed.
    pub query_only: bool,
    pub title: String,
    pub section_title: String,
    pub icon: &'static str,
    pub shortcut: String,
    pub run: Run,
}

/// Registry order: group rank, group, rank, id.
pub fn compare_contributions(a: &Contribution, b: &Contribution) -> Ordering {
    a.group_rank
        .cmp(&b.group_rank)
        .then_with(|| a.group.cmp(&b.group))
        .then(a.rank.cmp(&b.rank))
        .then_with(|| a.id.cmp(&b.id))
}

#[derive(Clone, Debug)]
pub struct Section {
    pub id: String,
    /// 0: contribution sections that reorder by match quality; 1: pinned below them.
    pub band: i64,
    pub rank: i64,
    pub title: Option<String>,
    pub results: Vec<ResultRow>,
    pub score: Option<Score>,
}

fn compare_sections(a: &Section, b: &Section) -> Ordering {
    a.band
        .cmp(&b.band)
        .then_with(|| match (&a.score, &b.score) {
            (Some(x), Some(y)) => compare(x, y),
            _ => Ordering::Equal,
        })
        .then(a.rank.cmp(&b.rank))
        .then_with(|| a.id.cmp(&b.id))
}

/// `buildContributionSections`: query-only rows drop out with no query; with one, the score
/// filters and both rows and sections sort by it.
pub fn contribution_sections(contributions: &[Contribution], query: &str) -> Vec<Section> {
    let has_query = !query.trim().is_empty();
    let mut sections: Vec<Section> = Vec::new();
    let mut registry: Vec<(String, usize)> = Vec::new();
    for (index, contribution) in contributions.iter().enumerate() {
        if contribution.query_only && !has_query {
            continue;
        }
        let score = if has_query {
            match score_fields(query, &[&contribution.title, ""], &contribution.keywords) {
                Some(score) => Some(score),
                None => continue,
            }
        } else {
            None
        };
        let position = match sections.iter().position(|s| s.id == contribution.group) {
            Some(position) => position,
            None => {
                sections.push(Section {
                    id: contribution.group.clone(),
                    band: 0,
                    rank: contribution.group_rank,
                    title: Some(contribution.section_title.clone()),
                    results: Vec::new(),
                    score: None,
                });
                sections.len() - 1
            }
        };
        registry.push((contribution.id.clone(), index));
        let section = &mut sections[position];
        let mut row = ResultRow::new(
            contribution.id.clone(),
            Kind::Action,
            contribution.title.clone(),
            contribution.run.clone(),
        );
        row.shortcut = contribution.shortcut.clone();
        row.icon = Icon::Glyph(contribution.icon);
        row.score = score;
        section.results.push(row);
        if let Some(score) = score
            && section.score.is_none_or(|s| compare(&score, &s).is_lt())
        {
            section.score = Some(score);
        }
    }
    if !has_query {
        return sections;
    }
    let index_of = |id: &str| {
        registry
            .iter()
            .find(|(r, _)| r == id)
            .map(|(_, i)| *i)
            .unwrap_or(0)
    };
    for section in &mut sections {
        section.results.sort_by(|a, b| {
            let delta = match (&a.score, &b.score) {
                (Some(x), Some(y)) => compare(x, y),
                _ => Ordering::Equal,
            };
            delta.then_with(|| index_of(&a.id).cmp(&index_of(&b.id)))
        });
    }
    sections.sort_by(compare_sections);
    sections
}

/// One projected list row (`projectCommandCenterRows` over `list-metrics.ts`).
#[derive(Clone, Debug)]
pub enum ListRow {
    Section {
        title: Option<String>,
        divider: bool,
    },
    Result(Box<ResultRow>),
}

pub struct Projection {
    pub rows: Vec<ListRow>,
    /// Row index of each selectable result, in order.
    pub selectable: Vec<usize>,
}

pub fn project_rows(mut sections: Vec<Section>) -> Projection {
    sections.retain(|s| !s.results.is_empty());
    sections.sort_by(compare_sections);
    let mut rows = Vec::new();
    let mut selectable = Vec::new();
    for (index, section) in sections.into_iter().enumerate() {
        let divider = index > 0;
        if divider || section.title.is_some() {
            rows.push(ListRow::Section {
                title: section.title,
                divider,
            });
        }
        for result in section.results {
            selectable.push(rows.len());
            rows.push(ListRow::Result(Box::new(result)));
        }
    }
    Projection { rows, selectable }
}

/// A section's rule band: 8px either side of the 1px rule.
pub const SECTION_DIVIDER_HEIGHT: f64 = 17.0;
/// A section's small muted label line.
pub const SECTION_TITLE_HEIGHT: f64 = 32.0;

/// Vertical result-row padding: 6px, or 12px so compact rows are 44px touch targets.
pub fn row_inset(compact: bool) -> f64 {
    if compact { 12.0 } else { 6.0 }
}

/// The fixed height each row is drawn at, so the panel can size to its rows and reveal one
/// exactly without measuring.
pub fn row_height(row: &ListRow, compact: bool) -> f64 {
    match row {
        ListRow::Section { title, divider } => {
            let rule = if *divider {
                SECTION_DIVIDER_HEIGHT
            } else {
                0.0
            };
            let label = if title.as_deref().is_some_and(|t| !t.is_empty()) {
                SECTION_TITLE_HEIGHT
            } else {
                0.0
            };
            rule + label
        }
        // One 20px line, or an 18px title over a 16px subtitle.
        ListRow::Result(result) => {
            let lines = if result.two_line() { 34.0 } else { 20.0 };
            lines + 2.0 * row_inset(compact)
        }
    }
}

/// The list viewport for rows of `heights` under `max`: all of them when they fit, else the
/// leading rows that fit whole, so the unscrolled list never ends mid-row.
pub fn list_height(heights: &[f64], max: f64) -> f64 {
    let max = max.max(0.0);
    let mut total = 0.0;
    for height in heights {
        if total + height > max + 0.5 {
            // A first row taller than the room still gets the room.
            return if total > 0.0 { total } else { max };
        }
        total += height;
    }
    total
}

/// The list anchor `(first_id, first_scroll)` that places content offset `y` at the top.
fn anchor_at(heights: &[f64], y: f64) -> (usize, f64) {
    let mut top = 0.0;
    for (index, height) in heights.iter().enumerate() {
        if y < top + height - 0.01 {
            return (index, top - y);
        }
        top += height;
    }
    let last = heights.len().saturating_sub(1);
    (last, top - heights.get(last).copied().unwrap_or(0.0) - y)
}

/// The anchor that shows rows `top..=bottom` whole in a `viewport`-tall list currently at
/// `(first_id, first_scroll)`, moving as little as possible; `None` when they already are.
/// A span taller than the viewport shows its top.
pub fn reveal_anchor(
    heights: &[f64],
    (first_id, first_scroll): (usize, f64),
    viewport: f64,
    top: usize,
    bottom: usize,
) -> Option<(usize, f64)> {
    let offset = |index: usize| heights[..index.min(heights.len())].iter().sum::<f64>();
    let total = offset(heights.len());
    let current = offset(first_id) - first_scroll;
    let start = offset(top);
    let end = offset(bottom + 1);
    let target = if start < current - 0.5 {
        start
    } else if end > current + viewport + 0.5 {
        (end - viewport).min(start)
    } else {
        return None;
    };
    Some(anchor_at(
        heights,
        target.clamp(0.0, (total - viewport).max(0.0)),
    ))
}

/// The rows to keep in view for the selected row: the row and, when it opens its section, the
/// section header above it.
pub fn reveal_span(rows: &[ListRow], index: usize) -> (usize, usize) {
    let top = match index.checked_sub(1).and_then(|i| rows.get(i)) {
        Some(ListRow::Section { .. }) => index - 1,
        _ => index,
    };
    (top, index)
}

/// `_joinSubtitle`.
pub fn join_subtitle(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .filter(|p| !p.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `describeWorkspaceFilePath`: (path, name, directory).
pub fn describe_file_path(path: &str) -> (String, String, String) {
    let normalized = path.replace('\\', "/");
    match normalized.rfind('/') {
        Some(index) => (
            normalized.clone(),
            normalized[index + 1..].to_owned(),
            normalized[..index].to_owned(),
        ),
        None => (normalized.clone(), normalized.clone(), String::new()),
    }
}

/// `resolveAgentLabel`: blank and the placeholder "New agent" have no label.
pub fn agent_label(title: &str) -> Option<String> {
    let trimmed = title.trim();
    (!trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("new agent")).then(|| trimmed.to_owned())
}

/// `pickAttentionAgent`: the top-level attention agent with the most urgent reason
/// (permission, error, finished), oldest first; the first listed wins a tie.
pub fn pick_attention_agent<'a>(agents: impl Iterator<Item = &'a Value>) -> Option<&'a Value> {
    use crate::features::workspace::rpc::str_of;
    let mut selected: Option<(i64, i64, &'a Value)> = None;
    for agent in agents {
        if agent.get("requiresAttention") != Some(&Value::Bool(true))
            || !str_of(agent, "parentAgentId").is_empty()
        {
            continue;
        }
        let priority = match str_of(agent, "attentionReason") {
            "permission" => 0,
            "error" => 1,
            "finished" => 2,
            _ => continue,
        };
        let timestamp = super::parse_ms(agent.get("attentionTimestamp")).unwrap_or(i64::MAX);
        if selected
            .as_ref()
            .is_none_or(|(p, t, _)| priority < *p || (priority == *p && timestamp < *t))
        {
            selected = Some((priority, timestamp, agent));
        }
    }
    selected.map(|(_, _, agent)| agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, title: &str, at: i64) -> ResultRow {
        let mut row = ResultRow::new(id, Kind::Agent, title, Run::Error(String::new()));
        row.activity_at = Some(at);
        row
    }

    #[test]
    fn built_in_rows_take_five_most_recent_without_query() {
        let rows = (0..7).map(|i| row(&i.to_string(), "x", i)).collect();
        let ranked = rank_built_in(rows, "");
        assert_eq!(
            ranked.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["6", "5", "4", "3", "2"]
        );
    }

    #[test]
    fn attention_priority_and_age() {
        let agents = [
            serde_json::json!({"id": "f", "requiresAttention": true, "attentionReason": "finished"}),
            serde_json::json!({"id": "e2", "requiresAttention": true, "attentionReason": "error", "attentionTimestamp": "2026-01-02T00:00:00Z"}),
            serde_json::json!({"id": "e1", "requiresAttention": true, "attentionReason": "error", "attentionTimestamp": "2026-01-01T00:00:00Z"}),
        ];
        assert_eq!(pick_attention_agent(agents.iter()), Some(&agents[2]));
    }

    #[test]
    fn attention_selection_preserves_identity_when_ids_collide() {
        let agents = [
            serde_json::json!({"id": "same", "requiresAttention": true, "attentionReason": "finished"}),
            serde_json::json!({"id": "same", "requiresAttention": true, "attentionReason": "permission"}),
        ];
        let chosen = pick_attention_agent(agents.iter()).unwrap();
        assert!(std::ptr::eq(chosen, &agents[1]));
    }

    fn section(title: Option<&str>, divider: bool) -> ListRow {
        ListRow::Section {
            title: title.map(str::to_owned),
            divider,
        }
    }

    fn action(id: &str) -> ListRow {
        ListRow::Result(Box::new(ResultRow::new(
            id,
            Kind::Action,
            id,
            Run::Error(String::new()),
        )))
    }

    #[test]
    fn row_heights_are_fixed_and_compact_rows_are_touch_targets() {
        let session = ListRow::Result(Box::new(row("s", "x", 0)));
        let file = ListRow::Result(Box::new(ResultRow::new(
            "f",
            Kind::File,
            "a.rs",
            Run::Error(String::new()),
        )));
        assert_eq!(row_height(&section(Some("A"), false), false), 32.0);
        assert_eq!(row_height(&section(Some("A"), true), false), 49.0);
        assert_eq!(row_height(&section(None, true), false), 17.0);
        assert_eq!(row_height(&action("a"), false), 32.0);
        assert_eq!(row_height(&session, false), 46.0);
        for row in [action("a"), session, file] {
            assert!(row_height(&row, true) >= 44.0);
        }
    }

    /// The reported palette: 9 actions under a titled section, then a divided "Sessions"
    /// section of two-line session rows. Every cap must end the list on a row boundary.
    #[test]
    fn whole_row_cap_with_mixed_rows_and_section_headers() {
        let section_of = |id: &str, band: i64, results: Vec<ResultRow>| Section {
            id: id.into(),
            band,
            rank: 0,
            title: Some(id.into()),
            results,
            score: None,
        };
        let actions = (0..9)
            .map(|i| {
                ResultRow::new(
                    format!("a{i}"),
                    Kind::Action,
                    "x",
                    Run::Error(String::new()),
                )
            })
            .collect();
        let sessions = vec![row("hello", "hello", 2), row("stream", "stream test", 1)];
        let projection = project_rows(vec![
            section_of("Actions", 0, actions),
            section_of("Sessions", 1, sessions),
        ]);
        for compact in [false, true] {
            let heights: Vec<f64> = projection
                .rows
                .iter()
                .map(|r| row_height(r, compact))
                .collect();
            let boundaries: Vec<f64> = heights
                .iter()
                .scan(0.0, |sum, h| {
                    *sum += h;
                    Some(*sum)
                })
                .collect();
            let total = *boundaries.last().unwrap();
            if !compact {
                // 32 + 9 × 32 + (17 + 32) + 2 × 46.
                assert_eq!(total, 461.0);
                // The reported 449px of room ends after "hello", not inside "stream test".
                assert_eq!(list_height(&heights, 449.0), 415.0);
            }
            let mut room = heights[0];
            while room <= total + 10.0 {
                let height = list_height(&heights, room);
                assert!(height <= room + 0.5, "{height} over {room}");
                assert!(
                    boundaries.contains(&height),
                    "{height} is not a row boundary (compact {compact})"
                );
                room += 1.0;
            }
        }
    }

    #[test]
    fn list_height_fits_content_or_ends_on_a_row_boundary() {
        let heights = [32.0, 32.0, 46.0, 46.0];
        assert_eq!(list_height(&heights, 500.0), 156.0);
        assert_eq!(list_height(&heights, 120.0), 110.0);
        assert_eq!(list_height(&heights, 20.0), 20.0);
        assert_eq!(list_height(&[], 300.0), 0.0);
    }

    #[test]
    fn reveal_keeps_rows_whole_and_shows_a_leading_section_header() {
        let heights = [32.0, 32.0, 32.0, 49.0, 46.0, 46.0];
        // Already whole at the top.
        assert_eq!(reveal_anchor(&heights, (0, 0.0), 100.0, 0, 1), None);
        // A header half under the top edge is scrolled back to the top.
        assert_eq!(
            reveal_anchor(&heights, (0, -10.0), 100.0, 0, 1),
            Some((0, 0.0))
        );
        // Moving down bottom-aligns the row: content offset 191 - 100 = 91, inside row 2.
        assert_eq!(
            reveal_anchor(&heights, (0, 0.0), 100.0, 3, 4),
            Some((2, -27.0))
        );
        // Moving up to a section's first row shows its header from the top.
        assert_eq!(
            reveal_anchor(&heights, (5, 0.0), 100.0, 3, 4),
            Some((3, 0.0))
        );
        // Never scrolls past the end of the content (237 - 100 = 137, inside row 3).
        assert_eq!(
            reveal_anchor(&heights, (0, 0.0), 100.0, 5, 5),
            Some((3, -41.0))
        );
    }

    #[test]
    fn reveal_span_includes_the_section_header() {
        let rows = [section(Some("A"), false), action("a"), action("b")];
        assert_eq!(reveal_span(&rows, 1), (0, 1));
        assert_eq!(reveal_span(&rows, 2), (2, 2));
    }
}
