//! When a ZWJ/ZWNJ in file content actually changes how letters are drawn.
//! Anywhere else it draws nothing and could make two different words look
//! the same, so the caller escapes it. Joining types come from Unicode's
//! ArabicShaping.txt and consonants from IndicSyllabicCategory.txt (18.0);
//! the tables cover only the blocks below. No character in them is a joiner
//! or in the strict escape set, so a run of joiners, or a joiner next to a
//! bidi or invisible character, stays escaped.
//!
//! The basis is UTS #39 "Unicode Security Mechanisms", section 3.1.1.1
//! "Limited Contexts for Joining Controls" (A1, A2 and B, the rules IDNA
//! uses), with two documented deviations:
//! - one extension: a ZWJ between a consonant and a virama is kept in
//!   Devanagari, Bengali and Sinhala only, where common fonts draw it
//!   distinctly (eyelash ra, ya-phala, touching letters);
//! - tighter restrictions, which the standard allows: after a virama only a
//!   consonant of the same script may follow, Myanmar asat and Sinhala
//!   al-lakuna act only with a ZWJ, Myanmar's invisible stacker (U+1039)
//!   does not count as a virama, and A1 does not look past transparent
//!   marks next to the ZWNJ.

const ZWNJ: char = '\u{200C}';
const ZWJ: char = '\u{200D}';

/// Whether `joiner` (ZWJ or ZWNJ) between `before` and `after` changes how
/// they are shaped. Three cases do:
/// - after a virama and before a consonant of its script, it picks the
///   conjunct, half form or chillu (UTS #39 A2 and B);
/// - a ZWJ between a consonant and a virama of Devanagari, Bengali or
///   Sinhala gives the eyelash ra, ya-phala or touching form (the extension);
/// - a ZWNJ after a dual-joining letter and before a dual- or right-joining
///   letter of the same script (Arabic, Syriac, N'Ko or Mongolian) breaks
///   the join (A1). A ZWJ there draws the plain pair.
pub fn shapes(joiner: char, before: char, after: char) -> bool {
    let after_virama = is_virama(before)
        && is_consonant(after)
        && same_indic_script(before, after)
        && (joiner == ZWJ || !matches!(before, '\u{0DCA}' | '\u{103A}'));
    let before_virama = joiner == ZWJ
        && is_consonant(before)
        && is_virama(after)
        && same_indic_script(before, after)
        && matches!(after, '\u{094D}' | '\u{09CD}' | '\u{0DCA}');
    let breaks_join = joiner == ZWNJ
        && is_dual_joining(before)
        && (is_dual_joining(after) || is_right_joining(after))
        && same_cursive_script(before, after);
    after_virama || before_virama || breaks_join
}

/// Indic scripts each fill one 128-code-point block; Myanmar's block is
/// 1000–109F.
fn same_indic_script(a: char, b: char) -> bool {
    let script = |c: char| match c {
        '\u{0900}'..='\u{0DFF}' => Some(u32::from(c) >> 7),
        '\u{1000}'..='\u{109F}' => Some(0x1000),
        _ => None,
    };
    script(a).is_some() && script(a) == script(b)
}

/// Letters of two scripts never join: shapers split text into runs by
/// script, so a ZWNJ between them breaks nothing.
fn same_cursive_script(a: char, b: char) -> bool {
    #[derive(PartialEq)]
    enum Cursive {
        Arabic,
        Syriac,
        Nko,
        Mongolian,
    }
    let script = |c: char| match c {
        '\u{0700}'..='\u{074F}' | '\u{0860}'..='\u{086F}' => Some(Cursive::Syriac),
        '\u{0600}'..='\u{077F}' | '\u{0870}'..='\u{08FF}' => Some(Cursive::Arabic),
        '\u{07C0}'..='\u{07FF}' => Some(Cursive::Nko),
        '\u{1800}'..='\u{18AF}' => Some(Cursive::Mongolian),
        _ => None,
    };
    script(a).is_some() && script(a) == script(b)
}

/// The virama of each Indic block, plus Myanmar's asat. Myanmar's invisible
/// stacker (U+1039) is left out: a joiner after it only breaks the stack.
fn is_virama(c: char) -> bool {
    matches!(
        c,
        '\u{094D}'
            | '\u{09CD}'
            | '\u{0A4D}'
            | '\u{0ACD}'
            | '\u{0B4D}'
            | '\u{0BCD}'
            | '\u{0C4D}'
            | '\u{0CCD}'
            | '\u{0D4D}'
            | '\u{0DCA}'
            | '\u{103A}'
    )
}

/// Indic_Syllabic_Category=Consonant in Devanagari to Sinhala and Myanmar.
fn is_consonant(c: char) -> bool {
    matches!(
        c,
        '\u{0915}'..='\u{0939}'
            | '\u{0958}'..='\u{095F}'
            | '\u{0978}'..='\u{097F}'
            | '\u{0995}'..='\u{09A8}'
            | '\u{09AA}'..='\u{09B0}'
            | '\u{09B2}'
            | '\u{09B6}'..='\u{09B9}'
            | '\u{09DC}'..='\u{09DD}'
            | '\u{09DF}'
            | '\u{09F0}'..='\u{09F1}'
            | '\u{0A15}'..='\u{0A28}'
            | '\u{0A2A}'..='\u{0A30}'
            | '\u{0A32}'..='\u{0A33}'
            | '\u{0A35}'..='\u{0A36}'
            | '\u{0A38}'..='\u{0A39}'
            | '\u{0A59}'..='\u{0A5C}'
            | '\u{0A5E}'
            | '\u{0A95}'..='\u{0AA8}'
            | '\u{0AAA}'..='\u{0AB0}'
            | '\u{0AB2}'..='\u{0AB3}'
            | '\u{0AB5}'..='\u{0AB9}'
            | '\u{0AF9}'
            | '\u{0B15}'..='\u{0B28}'
            | '\u{0B2A}'..='\u{0B30}'
            | '\u{0B32}'..='\u{0B33}'
            | '\u{0B35}'..='\u{0B39}'
            | '\u{0B5C}'..='\u{0B5D}'
            | '\u{0B5F}'
            | '\u{0B71}'
            | '\u{0B95}'
            | '\u{0B99}'..='\u{0B9A}'
            | '\u{0B9C}'
            | '\u{0B9E}'..='\u{0B9F}'
            | '\u{0BA3}'..='\u{0BA4}'
            | '\u{0BA8}'..='\u{0BAA}'
            | '\u{0BAE}'..='\u{0BB9}'
            | '\u{0C15}'..='\u{0C28}'
            | '\u{0C2A}'..='\u{0C39}'
            | '\u{0C58}'..='\u{0C5A}'
            | '\u{0C95}'..='\u{0CA8}'
            | '\u{0CAA}'..='\u{0CB3}'
            | '\u{0CB5}'..='\u{0CB9}'
            | '\u{0CDE}'
            | '\u{0D15}'..='\u{0D3A}'
            | '\u{0D9A}'..='\u{0DB1}'
            | '\u{0DB3}'..='\u{0DBB}'
            | '\u{0DBD}'
            | '\u{0DC0}'..='\u{0DC6}'
            | '\u{1000}'..='\u{1020}'
            | '\u{103F}'
            | '\u{1050}'..='\u{1051}'
            | '\u{105A}'..='\u{105D}'
            | '\u{1061}'
            | '\u{1065}'..='\u{1066}'
            | '\u{106E}'..='\u{1070}'
            | '\u{1075}'..='\u{1081}'
            | '\u{108E}'
    )
}

/// Joining_Type D: joins on both sides.
fn is_dual_joining(c: char) -> bool {
    matches!(
        c,
        '\u{0620}'
            | '\u{0626}'
            | '\u{0628}'
            | '\u{062A}'..='\u{062E}'
            | '\u{0633}'..='\u{063F}'
            | '\u{0641}'..='\u{0647}'
            | '\u{0649}'..='\u{064A}'
            | '\u{066E}'..='\u{066F}'
            | '\u{0678}'..='\u{0687}'
            | '\u{069A}'..='\u{06BF}'
            | '\u{06C1}'..='\u{06C2}'
            | '\u{06CC}'
            | '\u{06CE}'
            | '\u{06D0}'..='\u{06D1}'
            | '\u{06FA}'..='\u{06FC}'
            | '\u{06FF}'
            | '\u{0712}'..='\u{0714}'
            | '\u{071A}'..='\u{071D}'
            | '\u{071F}'..='\u{0727}'
            | '\u{0729}'
            | '\u{072B}'
            | '\u{072D}'..='\u{072E}'
            | '\u{074E}'..='\u{0758}'
            | '\u{075C}'..='\u{076A}'
            | '\u{076D}'..='\u{0770}'
            | '\u{0772}'
            | '\u{0775}'..='\u{0777}'
            | '\u{077A}'..='\u{077F}'
            | '\u{07CA}'..='\u{07EA}'
            | '\u{0860}'
            | '\u{0862}'..='\u{0865}'
            | '\u{0868}'
            | '\u{0886}'
            | '\u{0889}'..='\u{088D}'
            | '\u{088F}'
            | '\u{08A0}'..='\u{08A9}'
            | '\u{08AF}'..='\u{08B0}'
            | '\u{08B3}'..='\u{08B8}'
            | '\u{08BA}'..='\u{08C8}'
            | '\u{1820}'..='\u{1878}'
            | '\u{1887}'..='\u{18A8}'
            | '\u{18AA}'
    )
}

/// Joining_Type R: joins only to the letter before it.
fn is_right_joining(c: char) -> bool {
    matches!(
        c,
        '\u{0622}'..='\u{0625}'
            | '\u{0627}'
            | '\u{0629}'
            | '\u{062F}'..='\u{0632}'
            | '\u{0648}'
            | '\u{0671}'..='\u{0673}'
            | '\u{0675}'..='\u{0677}'
            | '\u{0688}'..='\u{0699}'
            | '\u{06C0}'
            | '\u{06C3}'..='\u{06CB}'
            | '\u{06CD}'
            | '\u{06CF}'
            | '\u{06D2}'..='\u{06D3}'
            | '\u{06D5}'
            | '\u{06EE}'..='\u{06EF}'
            | '\u{0710}'
            | '\u{0715}'..='\u{0719}'
            | '\u{071E}'
            | '\u{0728}'
            | '\u{072A}'
            | '\u{072C}'
            | '\u{072F}'
            | '\u{074D}'
            | '\u{0759}'..='\u{075B}'
            | '\u{076B}'..='\u{076C}'
            | '\u{0771}'
            | '\u{0773}'..='\u{0774}'
            | '\u{0778}'..='\u{0779}'
            | '\u{0867}'
            | '\u{0869}'..='\u{086A}'
            | '\u{0870}'..='\u{0882}'
            | '\u{088E}'
            | '\u{08AA}'..='\u{08AC}'
            | '\u{08AE}'
            | '\u{08B1}'..='\u{08B2}'
            | '\u{08B9}'
    )
}
