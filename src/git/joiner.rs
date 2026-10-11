//! When a ZWJ/ZWNJ in file content actually changes how letters are drawn.
//! Anywhere else it draws nothing and could make two different words look
//! the same, so the caller escapes it. Joining types come from Unicode's
//! ArabicShaping.txt (18.0); the tables cover only the blocks below. No
//! character in them is a joiner or in the strict escape set, so a run of
//! joiners, or a joiner next to a bidi or invisible character, stays escaped.

/// Whether a joiner between `before` and `after` changes how they are
/// shaped: next to a virama or asat in an Indic or Myanmar cluster, or
/// between Arabic-script, Syriac, N'Ko or Mongolian letters that join on
/// the sides facing it.
pub fn shapes(before: char, after: char) -> bool {
    let indic = (is_virama(before) && is_indic(after)) || (is_indic(before) && is_virama(after));
    indic || (joins_forward(before) && joins_backward(after))
}

/// Brahmic blocks (Devanagari to Sinhala) and Myanmar.
fn is_indic(c: char) -> bool {
    matches!(c, '\u{0900}'..='\u{0DFF}' | '\u{1000}'..='\u{109F}')
}

/// The virama of each Indic block, plus Myanmar's visible and invisible ones.
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
            | '\u{1039}'
            | '\u{103A}'
    )
}

/// Connects to the letter after it: Joining_Type D (dual) or C (causing,
/// such as tatweel).
fn joins_forward(c: char) -> bool {
    is_dual_joining(c) || is_join_causing(c)
}

/// Connects to the letter before it: Joining_Type D, R (right) or C.
fn joins_backward(c: char) -> bool {
    is_dual_joining(c) || is_join_causing(c) || is_right_joining(c)
}

fn is_join_causing(c: char) -> bool {
    matches!(c, '\u{0640}' | '\u{07FA}' | '\u{0883}'..='\u{0885}')
}

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
