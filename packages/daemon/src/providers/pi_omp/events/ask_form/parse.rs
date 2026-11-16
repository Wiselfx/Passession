//! Field model of an OMP `ask` form, mirroring OMP's `packages/tui/src/html-form/parse.ts`
//! (`Rw`) and the dialog titles of `data.ts` (`Iu`).
//!
//! OMP's RPC host receives the form only as one dialog per field, so these rules predict the
//! dialogs Paseo answers. The replay checks every dialog against them: a divergence falls back
//! to answering step by step, never to a wrong answer.

use super::dom::{Child, Dom, Element};
use std::collections::{HashMap, HashSet};

const CONTROLS: [&str; 3] = ["input", "select", "textarea"];
const SKIPPED: [&str; 10] = [
    "button", "head", "iframe", "link", "meta", "noscript", "script", "style", "template", "title",
];
const BLOCKS: [&str; 17] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "figure",
    "figcaption",
    "footer",
    "header",
    "li",
    "main",
    "nav",
    "p",
];
const HEADINGS: [&str; 7] = ["h1", "h2", "h3", "h4", "h5", "h6", "summary"];

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Block {
    /// Prose; the latest one titles an unlabelled field.
    Text(String),
    /// Lists, tables, preformatted text and images: shown, never a title.
    Other(String),
    Field(usize),
}

#[derive(Debug, Clone, Default)]
pub(super) struct Section {
    pub title: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub(super) struct Field {
    pub name: String,
    pub label: String,
    pub required: bool,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub(super) enum Kind {
    Choice {
        multi: bool,
        select: bool,
        other: bool,
        options: Vec<Choice>,
    },
    Text {
        multiline: bool,
        value: String,
        placeholder: String,
    },
}

#[derive(Debug, Clone)]
pub(super) struct Choice {
    pub value: String,
    pub label: String,
    pub checked: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Form {
    pub sections: Vec<Section>,
    pub fields: Vec<Field>,
}

impl Form {
    /// OMP's dialog title per field (`Iu`): its label, else the prose right before it, the
    /// section title, a lone option's label, then the name.
    pub(super) fn titles(&self) -> Vec<String> {
        let mut titles = vec![String::new(); self.fields.len()];
        for section in &self.sections {
            let mut previous = String::new();
            for block in &section.blocks {
                match block {
                    Block::Field(index) => {
                        let field = &self.fields[*index];
                        let lone = match &field.kind {
                            Kind::Choice { options, .. } if options.len() == 1 => {
                                options[0].label.as_str()
                            }
                            _ => "",
                        };
                        titles[*index] = [
                            field.label.as_str(),
                            previous.as_str(),
                            section.title.as_str(),
                            lone,
                            field.name.as_str(),
                        ]
                        .into_iter()
                        .map(collapse)
                        .find(|title| !title.is_empty())
                        .unwrap_or_default();
                        previous.clear();
                    }
                    Block::Text(text) => previous = text.clone(),
                    Block::Other(_) => previous.clear(),
                }
            }
        }
        titles
    }
}

pub(super) fn parse(html: &str) -> Form {
    let doc = Dom::parse_fragment(html);
    let root = doc.first("form").unwrap_or_else(|| doc.root_element());
    let ids: HashSet<&str> = root
        .descendants(&CONTROLS)
        .filter_map(|control| control.id())
        .collect();
    let mut label_for = HashMap::new();
    for label in root.descendants(&["label"]) {
        if let Some(target) = label.attr("for")
            && ids.contains(target)
        {
            label_for.entry(target).or_insert(label);
        }
    }
    let mut builder = Builder {
        label_for,
        groups: HashMap::new(),
        form: Form {
            sections: vec![Section::default()],
            fields: Vec::new(),
        },
        pending: String::new(),
        depth: 0,
    };
    builder.children(root);
    builder.flush();
    let mut form = builder.form;
    for field in &mut form.fields {
        if let Kind::Choice {
            select: false,
            other,
            options,
            ..
        } = &mut field.kind
            && options.len() >= 2
        {
            *other = !options.iter().any(|o| starts_with_other(&o.label));
        }
    }
    form
}

struct Builder<'a> {
    label_for: HashMap<&'a str, Element<'a>>,
    /// Radio/checkbox group (`type\0name`) to its field.
    groups: HashMap<String, usize>,
    form: Form,
    pending: String,
    depth: usize,
}

impl<'a> Builder<'a> {
    fn children(&mut self, el: Element<'a>) {
        for child in el.children() {
            match child {
                Child::Text(text) => self.pending.push_str(text),
                Child::Element(child) => self.visit(child),
            }
        }
    }

    fn visit(&mut self, el: Element<'a>) {
        let tag = el.name();
        if SKIPPED.contains(&tag) {
            return;
        }
        match tag {
            "br" => self.pending.push('\n'),
            "hr" => self.flush(),
            "input" | "select" | "textarea" => self.field(el, None),
            "label" => self.label(el),
            "fieldset" => self.fieldset(el),
            "legend" => {
                self.flush();
                self.children(el);
                self.flush();
            }
            "ul" | "ol" => self.list(el, tag == "ol"),
            "pre" => {
                self.flush();
                let text: String = el.text().collect();
                let text = text.strip_prefix('\n').unwrap_or(&text).trim_end();
                self.push_block(Block::Other(text.to_owned()));
            }
            "table" => self.table(el),
            "img" => {
                if el.attr("src").is_some_and(|src| !src.trim().is_empty()) {
                    self.flush();
                    let alt = el.attr("alt").unwrap_or("").trim().to_owned();
                    self.push_block(Block::Other(alt));
                }
            }
            "svg" => {
                self.flush();
                let alt = el
                    .attr("aria-label")
                    .map(|label| label.trim().to_owned())
                    .filter(|label| !label.is_empty())
                    .or_else(|| {
                        el.child_elements()
                            .find(|c| c.name() == "title")
                            .map(|title| collapse(&title.text().collect::<String>()))
                    })
                    .unwrap_or_default();
                self.push_block(Block::Other(alt));
            }
            _ if HEADINGS.contains(&tag) || BLOCKS.contains(&tag) => {
                self.flush();
                self.children(el);
                self.flush();
            }
            _ => self.children(el),
        }
    }

    fn flush(&mut self) {
        let text = normalize(&std::mem::take(&mut self.pending));
        if !text.is_empty() {
            self.push_block(Block::Text(text));
        }
    }

    fn push_block(&mut self, block: Block) {
        self.form
            .sections
            .last_mut()
            .expect("a current section")
            .blocks
            .push(block);
    }

    fn section(&mut self, title: String) {
        let current = self.form.sections.last_mut().expect("a current section");
        if current.blocks.is_empty() && current.title.is_empty() {
            current.title = title;
        } else {
            self.form.sections.push(Section {
                title,
                blocks: Vec::new(),
            });
        }
    }

    fn label(&mut self, el: Element<'a>) {
        if let Some(target) = el.attr("for")
            && self.label_for.get(target) == Some(&el)
        {
            return;
        }
        let controls: Vec<_> = el.descendants(&CONTROLS).collect();
        if controls.is_empty() {
            self.children(el);
            return;
        }
        self.flush();
        let text = text_of(el);
        for control in controls {
            self.field(control, Some(&text));
        }
    }

    fn fieldset(&mut self, el: Element<'a>) {
        self.flush();
        let legend = el.child_elements().find(|c| c.name() == "legend");
        if self.depth > 0 {
            if let Some(legend) = legend {
                self.visit(legend);
            }
        } else {
            let title = legend.map(|l| collapse(&text_of(l))).unwrap_or_default();
            self.section(title);
        }
        self.depth += 1;
        for child in el.children() {
            match child {
                Child::Text(text) => self.pending.push_str(text),
                Child::Element(child) if legend == Some(child) => {}
                Child::Element(child) => self.visit(child),
            }
        }
        self.flush();
        self.depth -= 1;
        if self.depth == 0 {
            self.section(String::new());
        }
    }

    fn list(&mut self, el: Element<'a>, ordered: bool) {
        self.flush();
        if el.descendants(&CONTROLS).next().is_some() {
            self.children(el);
            self.flush();
            return;
        }
        let items: Vec<String> = el
            .child_elements()
            .filter(|c| c.name() == "li")
            .map(text_of)
            .filter(|t| !t.is_empty())
            .collect();
        if !items.is_empty() {
            let text = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    if ordered {
                        format!("{}. {item}", i + 1)
                    } else {
                        format!("• {item}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.push_block(Block::Other(text));
        }
    }

    fn table(&mut self, table: Element<'a>) {
        self.flush();
        let cells = |row: Element<'a>| -> Vec<Element<'a>> {
            row.child_elements()
                .filter(|c| matches!(c.name(), "td" | "th"))
                .collect()
        };
        let rows: Vec<Element<'a>> = table
            .descendants(&["tr"])
            .filter(|row| closest(*row, "table") == Some(table))
            .collect();
        if table.descendants(&CONTROLS).next().is_none() {
            let lines: Vec<Vec<String>> = rows
                .iter()
                .map(|row| cells(*row).into_iter().map(text_of).collect())
                .collect();
            if lines.iter().any(|line| !line.is_empty()) {
                let text = lines
                    .iter()
                    .map(|line| line.join(" | "))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.push_block(Block::Other(text));
            }
            return;
        }
        let mut headers: Vec<String> = Vec::new();
        for row in rows {
            let row_cells = cells(row);
            if row_cells.iter().all(|c| c.name() == "th")
                && row.descendants(&CONTROLS).next().is_none()
            {
                headers = row_cells.into_iter().map(text_of).collect();
                continue;
            }
            let prose: Vec<String> = row_cells
                .iter()
                .filter(|cell| cell.descendants(&CONTROLS).next().is_none())
                .map(|cell| text_of(*cell))
                .filter(|t| !t.is_empty())
                .collect();
            if !prose.is_empty() {
                self.push_block(Block::Text(prose.join(" · ")));
            }
            for (column, cell) in row_cells.iter().enumerate() {
                let controls: Vec<_> = cell.descendants(&CONTROLS).collect();
                for control in controls {
                    let label = closest(control, "label")
                        .filter(|label| contains(*cell, *label))
                        .map_or_else(|| text_of(*cell), text_of);
                    let label = if label.is_empty() {
                        headers.get(column).cloned().unwrap_or_default()
                    } else {
                        label
                    };
                    self.field(control, Some(&label));
                }
            }
        }
    }

    fn field(&mut self, el: Element<'a>, label: Option<&str>) {
        self.flush();
        let label = label
            .map(collapse)
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| self.own_label(el));
        let name = el.attr("name").unwrap_or("").to_owned();
        let required = el.attr("required").is_some();
        let placeholder = el.attr("placeholder").unwrap_or("").to_owned();
        match el.name() {
            "select" => {
                let multi = el.attr("multiple").is_some();
                let mut options: Vec<Choice> = el
                    .descendants(&["option"])
                    .map(|option| {
                        let text = collapse(&option.text().collect::<String>());
                        Choice {
                            value: option
                                .attr("value")
                                .map_or_else(|| text.clone(), str::to_owned),
                            label: text,
                            checked: option.attr("selected").is_some(),
                            disabled: option.attr("disabled").is_some(),
                        }
                    })
                    .collect();
                if !multi
                    && !options.iter().any(|o| o.checked)
                    && let Some(first) = options.iter_mut().find(|o| !o.disabled)
                {
                    first.checked = true;
                }
                self.push(Field {
                    name,
                    label,
                    required,
                    kind: Kind::Choice {
                        multi,
                        select: true,
                        other: false,
                        options,
                    },
                });
            }
            "textarea" => {
                let raw: String = el.text().collect();
                let value = raw
                    .strip_prefix("\r\n")
                    .or_else(|| raw.strip_prefix('\n'))
                    .unwrap_or(&raw)
                    .to_owned();
                self.push(Field {
                    label: or_name(label, &name),
                    name,
                    required,
                    kind: Kind::Text {
                        multiline: true,
                        value,
                        placeholder,
                    },
                });
            }
            _ => {
                let input_type = el.attr("type").unwrap_or("text").to_ascii_lowercase();
                if matches!(
                    input_type.as_str(),
                    "button" | "file" | "image" | "reset" | "submit" | "hidden"
                ) {
                    return;
                }
                let value = el.attr("value");
                if matches!(input_type.as_str(), "radio" | "checkbox") {
                    let option = Choice {
                        value: value.unwrap_or("on").to_owned(),
                        label: if label.is_empty() {
                            collapse(value.unwrap_or(&name))
                        } else {
                            label
                        },
                        checked: el.attr("checked").is_some(),
                        disabled: el.attr("disabled").is_some(),
                    };
                    let key = format!("{input_type}\0{name}");
                    if let Some(&index) = self.groups.get(&key) {
                        let field = &mut self.form.fields[index];
                        field.required |= required;
                        if let Kind::Choice { options, .. } = &mut field.kind {
                            options.push(option);
                        }
                        return;
                    }
                    self.groups.insert(key, self.form.fields.len());
                    self.push(Field {
                        name,
                        label: String::new(),
                        required,
                        kind: Kind::Choice {
                            multi: input_type == "checkbox",
                            select: false,
                            other: false,
                            options: vec![option],
                        },
                    });
                    return;
                }
                self.push(Field {
                    label: or_name(label, &name),
                    name,
                    required,
                    kind: Kind::Text {
                        multiline: false,
                        value: value.unwrap_or("").to_owned(),
                        placeholder,
                    },
                });
            }
        }
    }

    /// `label[for]`, then `aria-label`, then `title`.
    fn own_label(&self, el: Element<'a>) -> String {
        if let Some(id) = el.id()
            && let Some(label) = self.label_for.get(id)
        {
            return collapse(&text_of(*label));
        }
        el.attr("aria-label")
            .or_else(|| el.attr("title"))
            .map(collapse)
            .unwrap_or_default()
    }

    fn push(&mut self, field: Field) {
        let index = self.form.fields.len();
        self.form.fields.push(field);
        self.push_block(Block::Field(index));
    }
}

fn closest<'a>(el: Element<'a>, tag: &str) -> Option<Element<'a>> {
    el.ancestors().find(|ancestor| ancestor.name() == tag)
}

fn contains(outer: Element<'_>, inner: Element<'_>) -> bool {
    outer == inner || inner.ancestors().any(|a| a == outer)
}

/// A label's text: controls and hidden elements excluded, `<br>` as a line break, images by
/// their alt text, block elements separated by a space.
fn text_of(el: Element<'_>) -> String {
    fn gather(el: Element<'_>, out: &mut String) {
        for child in el.children() {
            match child {
                Child::Text(text) => out.push_str(text),
                Child::Element(element) => {
                    let tag = element.name();
                    if SKIPPED.contains(&tag)
                        || matches!(tag, "input" | "select" | "textarea" | "svg")
                    {
                        continue;
                    }
                    match tag {
                        "br" => out.push('\n'),
                        "img" => out.push_str(element.attr("alt").unwrap_or("")),
                        _ => {
                            if BLOCKS.contains(&tag) || HEADINGS.contains(&tag) {
                                out.push(' ');
                            }
                            gather(element, out);
                        }
                    }
                }
            }
        }
    }
    let mut out = String::new();
    gather(el, &mut out);
    normalize(&out)
}

/// Collapses spaces but keeps line breaks, trimming both ends.
fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c == '\n' {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
            space = false;
        } else if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out.trim().to_owned()
}

pub(super) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn or_name(label: String, name: &str) -> String {
    if !label.is_empty() {
        return label;
    }
    let mut out = String::new();
    for c in name.chars() {
        if c == '_' || c == '-' {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// OMP's `/^\s*other\b/i`: an author-supplied "Other" option replaces the automatic one.
fn starts_with_other(label: &str) -> bool {
    let rest = label.trim_start();
    rest.get(..5)
        .is_some_and(|word| word.eq_ignore_ascii_case("other"))
        && !rest[5..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}
