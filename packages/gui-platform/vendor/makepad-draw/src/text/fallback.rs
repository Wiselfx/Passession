//! Bounded discovery handoff shared by UI and background text layout.
use super::font_family::FontFamilyId;
use std::{collections::VecDeque, sync::Mutex};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MissingGlyph {
    pub family: FontFamilyId,
    pub character: char,
}

#[derive(Debug, Default)]
struct Pending {
    queue: VecDeque<MissingGlyph>,
    // One Unicode bitmap per participating family, allocated on its first miss.
    // Unlike an LRU, this cannot retry negative discoveries forever on a
    // document containing more distinct missing characters than the cache fits.
    seen: Vec<Vec<u64>>,
    overflow: bool,
}

#[derive(Debug)]
pub struct FallbackRequests {
    families: Vec<FontFamilyId>,
    pending: Mutex<Pending>,
}

impl FallbackRequests {
    pub fn new(families: Vec<FontFamilyId>) -> Self {
        let seen = vec![Vec::new(); families.len()];
        Self { families, pending: Mutex::new(Pending { seen, ..Pending::default() }) }
    }

    pub fn request(&self, missing: MissingGlyph) {
        let Some(family) = self.families.iter().position(|family| *family == missing.family) else { return };
        if missing.character.is_control() { return; }
        let mut pending = self.pending.lock().unwrap();
        let codepoint = missing.character as usize;
        let (word, mask) = (codepoint / 64, 1u64 << (codepoint % 64));
        if pending.seen[family].is_empty() {
            pending.seen[family].resize(0x110000 / 64, 0);
        }
        if pending.seen[family][word] & mask != 0 { return; }
        if pending.queue.len() == 4096 {
            pending.overflow = true;
        } else {
            pending.seen[family][word] |= mask;
            pending.queue.push_back(missing);
        }
        makepad_platform::SignalToUI::set_ui_signal();
    }

    /// At most 128 OS probes per worker batch. Overflow requires a fresh layout
    /// after the batch, even if all probes were negative: omitted misses must be
    /// discovered again rather than stranded in a cached missing-glyph layout.
    pub fn drain(&self) -> (Vec<MissingGlyph>, bool) {
        let mut pending = self.pending.lock().unwrap();
        let count = pending.queue.len().min(128);
        let requests = pending.queue.drain(..count).collect();
        let overflow = std::mem::take(&mut pending.overflow);
        (requests, overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misses_are_deduplicated_and_new_epochs_retry_negatives() {
        let family = FontFamilyId::from(1);
        let requests = FallbackRequests::new(vec![family]);
        let missing = MissingGlyph { family, character: '𐐀' };
        requests.request(missing);
        requests.request(missing);
        assert_eq!(requests.drain(), (vec![missing], false));
        requests.request(missing);
        assert!(requests.drain().0.is_empty());
        let refreshed = FallbackRequests::new(vec![family]);
        refreshed.request(missing);
        assert_eq!(refreshed.drain().0, vec![missing]);
    }

    #[test]
    fn queue_overflow_does_not_mark_omitted_characters_resolved() {
        let family = FontFamilyId::from(2);
        let requests = FallbackRequests::new(vec![family]);
        for codepoint in 0x4000..0x5000 {
            requests.request(MissingGlyph { family, character: char::from_u32(codepoint).unwrap() });
        }
        let omitted = MissingGlyph { family, character: '𐐀' };
        requests.request(omitted);
        let (batch, overflow) = requests.drain();
        assert_eq!(batch.len(), 128);
        assert!(overflow);
        requests.request(omitted);
        let mut found = false;
        loop {
            let (batch, _) = requests.drain();
            if batch.is_empty() { break; }
            found |= batch.contains(&omitted);
        }
        assert!(found);
    }

    #[test]
    fn unavailable_glyphs_beyond_queue_capacity_eventually_quiesce() {
        let family = FontFamilyId::from(3);
        let requests = FallbackRequests::new(vec![family]);
        let mut discovered = 0;
        let mut quiescent = false;
        // Simulate entirely negative OS results and a fresh layout after each
        // batch. More than both the queue and the former 8192-entry LRU fit.
        for _ in 0..100 {
            for codepoint in 0x10000..0x10000 + 9000 {
                requests.request(MissingGlyph { family, character: char::from_u32(codepoint).unwrap() });
            }
            let (batch, overflow) = requests.drain();
            discovered += batch.len();
            if batch.is_empty() {
                assert!(!overflow);
                quiescent = true;
                break;
            }
        }
        assert!(quiescent, "negative results must stop requesting redraws");
        assert_eq!(discovered, 9000, "every scalar is discovered exactly once");
    }
}
