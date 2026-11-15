use {
    super::{
        font::Font,
        intern::Intern,
        shaper::{ShapeParams, ShapedText, Shaper},
        substr::Substr,
    },
    std::{
        cell::RefCell,
        hash::{Hash, Hasher},
        rc::Rc,
    },
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FontFamilyId(u64);

impl From<u64> for FontFamilyId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<&str> for FontFamilyId {
    fn from(value: &str) -> Self {
        Self(value.intern().as_ptr() as u64)
    }
}

#[derive(Debug)]
pub struct FontFamily {
    id: FontFamilyId,
    shaper: Rc<RefCell<Shaper>>,
    fonts: Rc<[Rc<Font>]>,
    fallback_requests: Rc<RefCell<Option<std::sync::Arc<super::fallback::FallbackRequests>>>>,
}

impl FontFamily {
    pub fn new(
        id: FontFamilyId,
        shaper: Rc<RefCell<Shaper>>,
        fonts: Rc<[Rc<Font>]>,
        fallback_requests: Rc<RefCell<Option<std::sync::Arc<super::fallback::FallbackRequests>>>>,
    ) -> Self {
        Self { id, shaper, fonts, fallback_requests }
    }

    pub fn id(&self) -> FontFamilyId {
        self.id
    }

    pub fn get_or_shape(&self, text: Substr) -> Rc<ShapedText> {
        use unicode_segmentation::UnicodeSegmentation;
        let shaped = self.shaper.borrow_mut().get_or_shape(ShapeParams {
            text,
            fonts: self.fonts.clone(),
        });
        if !shaped.has_missing_glyphs {
            return shaped;
        }
        if let Some(requests) = &*self.fallback_requests.borrow() {
            for glyph in shaped.glyphs.iter().filter(|glyph| glyph.id == 0) {
                if let Some(grapheme) = shaped.text[glyph.cluster..].graphemes(true).next() {
                    for character in grapheme.chars() {
                        requests.request(super::fallback::MissingGlyph { family: self.id, character });
                    }
                }
            }
        }
        shaped
    }

    pub fn fonts(&self) -> &[Rc<Font>] {
        &self.fonts
    }
}

impl Eq for FontFamily {}

impl Hash for FontFamily {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl PartialEq for FontFamily {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}
