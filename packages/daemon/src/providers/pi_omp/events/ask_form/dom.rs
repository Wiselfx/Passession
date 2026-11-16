//! The HTML5 tree the form parser walks: html5ever's tree builder (malformed-markup repair,
//! entities, foreign content) writing into an `ego_tree` arena through [`Sink`].
//!
//! The parser only matches tag names and attributes, so it needs no CSS selector engine. Template
//! contents hang off their `<template>` as a non-element first child, so walks still reach them.

use ego_tree::{NodeId, NodeMut, NodeRef, Tree};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, ParseOpts, QualName, local_name, ns};
use std::borrow::Cow;
use std::cell::{Ref, RefCell};

enum Node {
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
    },
    Text(StrTendril),
    /// The root, template contents, comments and processing instructions: never read, but
    /// they keep the tree's shape and stop text on either side from merging.
    Other,
}

pub(super) struct Dom(Tree<Node>);

impl Dom {
    /// Parses `html` as the children of a `<body>`, under one `<html>` element.
    pub(super) fn parse_fragment(html: &str) -> Self {
        let context = QualName::new(None, ns!(html), local_name!("body"));
        let sink = Sink(RefCell::new(Tree::new(Node::Other)));
        html5ever::parse_fragment(sink, ParseOpts::default(), context, Vec::new(), false).one(html)
    }

    /// The `<html>` element holding the fragment.
    pub(super) fn root_element(&self) -> Element<'_> {
        self.0
            .root()
            .children()
            .find_map(Element::wrap)
            .expect("fragment parsing creates an html element")
    }

    /// The first attached `tag` element in creation (arena) order, template contents included.
    pub(super) fn first(&self, tag: &str) -> Option<Element<'_>> {
        self.0
            .nodes()
            .filter(|node| node.parent().is_some())
            .filter_map(Element::wrap)
            .find(|el| el.name() == tag)
    }
}

/// One child of an element that the parser reads.
pub(super) enum Child<'a> {
    Text(&'a str),
    Element(Element<'a>),
}

#[derive(Clone, Copy)]
pub(super) struct Element<'a> {
    node: NodeRef<'a, Node>,
    name: &'a QualName,
    attrs: &'a [Attribute],
}

/// The same node of the same tree.
impl PartialEq for Element<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
    }
}

impl<'a> Element<'a> {
    fn wrap(node: NodeRef<'a, Node>) -> Option<Self> {
        match node.value() {
            Node::Element { name, attrs } => Some(Element { node, name, attrs }),
            _ => None,
        }
    }

    /// The local name; the namespace is ignored, as type selectors did.
    pub(super) fn name(self) -> &'a str {
        &self.name.local
    }

    /// A namespace-less attribute.
    pub(super) fn attr(self, name: &str) -> Option<&'a str> {
        self.attrs
            .iter()
            .find(|attr| attr.name.ns == ns!() && &*attr.name.local == name)
            .map(|attr| &*attr.value)
    }

    /// The `id` attribute in any namespace.
    pub(super) fn id(self) -> Option<&'a str> {
        self.attrs
            .iter()
            .find(|attr| &*attr.name.local == "id")
            .map(|attr| &*attr.value)
    }

    pub(super) fn children(self) -> impl Iterator<Item = Child<'a>> {
        self.node.children().filter_map(|child| match child.value() {
            Node::Text(text) => Some(Child::Text(&**text)),
            Node::Element { .. } => Element::wrap(child).map(Child::Element),
            Node::Other => None,
        })
    }

    pub(super) fn child_elements(self) -> impl Iterator<Item = Element<'a>> {
        self.node.children().filter_map(Element::wrap)
    }

    /// Descendant elements named one of `tags`, in document order, excluding `self`.
    pub(super) fn descendants(
        self,
        tags: &'static [&'static str],
    ) -> impl Iterator<Item = Element<'a>> {
        self.node
            .descendants()
            .skip(1)
            .filter_map(Element::wrap)
            .filter(move |el| tags.contains(&el.name()))
    }

    pub(super) fn ancestors(self) -> impl Iterator<Item = Element<'a>> {
        self.node.ancestors().filter_map(Element::wrap)
    }

    /// Every descendant text node, in document order.
    pub(super) fn text(self) -> impl Iterator<Item = &'a str> {
        self.node.descendants().filter_map(|node| match node.value() {
            Node::Text(text) => Some(&**text),
            _ => None,
        })
    }
}

/// html5ever's tree-builder callbacks onto the arena.
struct Sink(RefCell<Tree<Node>>);

/// Appends `text` to `node` when it is a text node, so adjacent text stays one node.
fn merge_text(node: Option<NodeMut<'_, Node>>, text: &StrTendril) -> bool {
    node.is_some_and(|mut node| match node.value() {
        Node::Text(existing) => {
            existing.push_tendril(text);
            true
        }
        _ => false,
    })
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Dom;
    type ElemName<'a> = Ref<'a, QualName>;

    fn finish(self) -> Dom {
        Dom(self.0.into_inner())
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        self.0.borrow().root().id()
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Ref<'a, QualName> {
        Ref::map(self.0.borrow(), |tree| {
            match tree.get(*target).map(|node| node.value()) {
                Some(Node::Element { name, .. }) => name,
                _ => unreachable!("html5ever asks element names of elements only"),
            }
        })
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> NodeId {
        let template = name.ns == ns!(html) && name.local == local_name!("template");
        let mut tree = self.0.borrow_mut();
        let mut node = tree.orphan(Node::Element { name, attrs });
        if template {
            node.append(Node::Other);
        }
        node.id()
    }

    fn create_comment(&self, _text: StrTendril) -> NodeId {
        self.0.borrow_mut().orphan(Node::Other).id()
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> NodeId {
        self.0.borrow_mut().orphan(Node::Other).id()
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let mut tree = self.0.borrow_mut();
        let mut parent = tree.get_mut(*parent).expect("live parent");
        match child {
            NodeOrText::AppendNode(id) => {
                parent.append_id(id);
            }
            NodeOrText::AppendText(text) => {
                if !merge_text(parent.last_child(), &text) {
                    parent.append(Node::Text(text));
                }
            }
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let attached = self
            .0
            .borrow()
            .get(*element)
            .is_some_and(|node| node.parent().is_some());
        if attached {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    /// The parser never reads a doctype.
    fn append_doctype_to_document(
        &self,
        _name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.0
            .borrow()
            .get(*target)
            .and_then(|template| template.first_child())
            .expect("template contents")
            .id()
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let mut tree = self.0.borrow_mut();
        if let NodeOrText::AppendNode(id) = new_node {
            tree.get_mut(id).expect("live node").detach();
        }
        let mut sibling = tree.get_mut(*sibling).expect("live sibling");
        if sibling.parent().is_none() {
            return;
        }
        match new_node {
            NodeOrText::AppendNode(id) => {
                sibling.insert_id_before(id);
            }
            NodeOrText::AppendText(text) => {
                if !merge_text(sibling.prev_sibling(), &text) {
                    sibling.insert_before(Node::Text(text));
                }
            }
        }
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut tree = self.0.borrow_mut();
        let mut node = tree.get_mut(*target).expect("live element");
        let Node::Element {
            attrs: existing, ..
        } = node.value()
        else {
            unreachable!("html5ever adds attributes to elements only");
        };
        for attr in attrs {
            if !existing.iter().any(|a| a.name == attr.name) {
                existing.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.0
            .borrow_mut()
            .get_mut(*target)
            .expect("live node")
            .detach();
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        self.0
            .borrow_mut()
            .get_mut(*new_parent)
            .expect("live parent")
            .reparent_from_id_append(*node);
    }
}
