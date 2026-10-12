//! Which characters can be part of an emoji ZWJ sequence, so a ZWJ between
//! two of them can be told apart from one that hides between letters.
//! The table is the Extended_Pictographic property from Unicode 16.0's
//! emoji-data.txt, written out by hand to avoid a dependency. It includes
//! unassigned code points the standard reserves for future emoji.

/// Whether `c` is Extended_Pictographic: an emoji, or a code point kept
/// for one, that a ZWJ may join to the next.
pub(crate) fn is_pictographic(c: char) -> bool {
    matches!(
        c,
        '\u{00A9}'
            | '\u{00AE}'
            | '\u{203C}'
            | '\u{2049}'
            | '\u{2122}'
            | '\u{2139}'
            | '\u{2194}'..='\u{2199}'
            | '\u{21A9}'..='\u{21AA}'
            | '\u{231A}'..='\u{231B}'
            | '\u{2328}'
            | '\u{2388}'
            | '\u{23CF}'
            | '\u{23E9}'..='\u{23F3}'
            | '\u{23F8}'..='\u{23FA}'
            | '\u{24C2}'
            | '\u{25AA}'..='\u{25AB}'
            | '\u{25B6}'
            | '\u{25C0}'
            | '\u{25FB}'..='\u{25FE}'
            | '\u{2600}'..='\u{2605}'
            | '\u{2607}'..='\u{2612}'
            | '\u{2614}'..='\u{2685}'
            | '\u{2690}'..='\u{2705}'
            | '\u{2708}'..='\u{2712}'
            | '\u{2714}'
            | '\u{2716}'
            | '\u{271D}'
            | '\u{2721}'
            | '\u{2728}'
            | '\u{2733}'..='\u{2734}'
            | '\u{2744}'
            | '\u{2747}'
            | '\u{274C}'
            | '\u{274E}'
            | '\u{2753}'..='\u{2755}'
            | '\u{2757}'
            | '\u{2763}'..='\u{2767}'
            | '\u{2795}'..='\u{2797}'
            | '\u{27A1}'
            | '\u{27B0}'
            | '\u{27BF}'
            | '\u{2934}'..='\u{2935}'
            | '\u{2B05}'..='\u{2B07}'
            | '\u{2B1B}'..='\u{2B1C}'
            | '\u{2B50}'
            | '\u{2B55}'
            | '\u{3030}'
            | '\u{303D}'
            | '\u{3297}'
            | '\u{3299}'
            | '\u{1F000}'..='\u{1F0FF}'
            | '\u{1F10D}'..='\u{1F10F}'
            | '\u{1F12F}'
            | '\u{1F16C}'..='\u{1F171}'
            | '\u{1F17E}'..='\u{1F17F}'
            | '\u{1F18E}'
            | '\u{1F191}'..='\u{1F19A}'
            | '\u{1F1AD}'..='\u{1F1E5}'
            | '\u{1F201}'..='\u{1F20F}'
            | '\u{1F21A}'
            | '\u{1F22F}'
            | '\u{1F232}'..='\u{1F23A}'
            | '\u{1F23C}'..='\u{1F23F}'
            | '\u{1F249}'..='\u{1F3FA}'
            | '\u{1F400}'..='\u{1F53D}'
            | '\u{1F546}'..='\u{1F64F}'
            | '\u{1F680}'..='\u{1F6FF}'
            | '\u{1F774}'..='\u{1F77F}'
            | '\u{1F7D5}'..='\u{1F7FF}'
            | '\u{1F80C}'..='\u{1F80F}'
            | '\u{1F848}'..='\u{1F84F}'
            | '\u{1F85A}'..='\u{1F85F}'
            | '\u{1F888}'..='\u{1F88F}'
            | '\u{1F8AE}'..='\u{1F8AF}'
            | '\u{1F8BC}'..='\u{1F8BF}'
            | '\u{1F8C2}'..='\u{1F8FF}'
            | '\u{1F90C}'..='\u{1F93A}'
            | '\u{1F93C}'..='\u{1F945}'
            | '\u{1F947}'..='\u{1FAFF}'
            | '\u{1FC00}'..='\u{1FFFD}'
    )
}

/// The five skin-tone modifiers, which attach to the emoji before them.
pub(crate) fn is_modifier(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// The 26 regional-indicator letters: two in a row draw as one flag.
pub(crate) fn is_regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_are_pictographic_and_letters_marks_and_modifiers_are_not() {
        for c in ['👨', '👩', '👧', '💻', '❤', '🔥', '🏳', '⚧', '🐻', '❄', '©']
        {
            assert!(is_pictographic(c), "{c:?}");
        }
        for c in [
            'a',
            '1',
            '#',
            'م',
            'क',
            '\u{200D}',
            '\u{FE0F}',
            '\u{1F3FB}',
            '\u{1F1E6}',
            '\u{E0041}',
        ] {
            assert!(!is_pictographic(c), "{c:?}");
        }
        assert!(is_modifier('\u{1F3FB}') && is_modifier('\u{1F3FF}'));
        assert!(!is_modifier('\u{1F3FA}') && !is_modifier('\u{1F400}'));
        assert!(!is_modifier('👨') && !is_modifier('\u{FE0F}'));
    }

    #[test]
    fn regional_indicators_are_the_26_letters_from_u1f1e6() {
        assert!(is_regional_indicator('\u{1F1E6}') && is_regional_indicator('\u{1F1FF}'));
        for c in ['\u{1F1E5}', '\u{1F200}', 'A'] {
            assert!(!is_regional_indicator(c), "{c:?}");
        }
    }

    #[test]
    fn the_table_edges_match_emoji_data() {
        // Each pair: the first and last code point of a range, then the
        // code points just outside it.
        for (inside, outside) in [
            (['\u{1F249}', '\u{1F3FA}'], ['\u{1F248}', '\u{1F3FB}']),
            (['\u{1F400}', '\u{1F53D}'], ['\u{1F3FF}', '\u{1F53E}']),
            (['\u{1F546}', '\u{1F64F}'], ['\u{1F545}', '\u{1F650}']),
            (['\u{1F680}', '\u{1F6FF}'], ['\u{1F67F}', '\u{1F700}']),
            (['\u{1F8AE}', '\u{1F8AF}'], ['\u{1F8AD}', '\u{1F8B0}']),
            (['\u{1F8BC}', '\u{1F8BF}'], ['\u{1F8BB}', '\u{1F8C0}']),
            (['\u{1F8C2}', '\u{1F8FF}'], ['\u{1F8C1}', '\u{1F900}']),
            (['\u{1F947}', '\u{1FAFF}'], ['\u{1F946}', '\u{1FB00}']),
            (['\u{1FC00}', '\u{1FFFD}'], ['\u{1FBFF}', '\u{1FFFE}']),
            (['\u{2614}', '\u{2685}'], ['\u{2613}', '\u{2686}']),
            (['\u{2690}', '\u{2705}'], ['\u{268F}', '\u{2706}']),
        ] {
            for c in inside {
                assert!(is_pictographic(c), "{c:?}");
            }
            for c in outside {
                assert!(!is_pictographic(c), "{c:?}");
            }
        }
    }
}
