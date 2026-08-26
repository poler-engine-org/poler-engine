//! Лёгкий стеммер кириллицы (укр + рос) для веб-индекса.
//!
//! История бага: запрос «ініціац» давал 0 хитов — в тексте «ініціація»,
//! «ініціації», «ініціацію»… Склонения славянских языков меняют окончания,
//! и точный токен-матч слеп к словоформам. Snowball-стеммеры точнее, но
//! тянут зависимость; здесь — suffix-stripping по курируемому списку
//! окончаний с защитой минимальной длины основы:
//!
//! * токены без кириллицы не трогаем (ownership == ownership);
//! * основа после отрезания обязана иметь ≥ MIN_STEM символов
//!   («мати» не превращается в «ма»);
//! * до двух проходов (комбинации окончаний: «книгами» → «книг»).
//!
//! Стем — всегда ПРЕФИКС словоформы, поэтому сниппет-подстрока
//! (snippet_for ищет term внутри текста) продолжает работать:
//! «доросл» содержится в «доросла» и «дорослий».

/// Минимальная длина СЛОВА для стемминга: короткие (≤4 симв.) не трогаем —
/// там основа слишком двусмысленна («мати»≠«мат», «вік»≠«віку» терпимо).
const MIN_WORD: usize = 5;

/// Минимальная длина основы после отрезания окончания (в символах).
const MIN_STEM: usize = 3;

/// Флективные окончания укр/рос (порядок не важен — ищем самый длинный).
const CYR_SUFFIXES: &[&str] = &[
    // дієслівні (найдовші)
    "уватися", "уються", "уватиму", "увалися", "уватиме", "иться", "еться", "утися", "атся",
    "ували", "увала", "увало", "уємо",
    // іменникові відмінкові
    "еннями", "еннях", "аннях", "ового", "енями",
    "ення", "ання", "ень", "нями",
    // прикметникові
    "ими", "ыми", "ейший", "ого", "ому", "ові", "еві",
    // короткі закінчення
    "ами", "ями", "ої", "ою", "ею", "ію", "ія", "ії", "ією", "ів", "ов", "ей", "ой", "ом",
    "ем", "ам", "ям", "ах", "ях", "ую", "ий", "ый", "ая", "яя", "ое",
    "ти", "ся", "сь", "ші", "ше",
    "и", "і", "а", "я", "у", "ю", "е", "о", "ь", "є",
];
// Прошедшее время «ла/ло/ли» сознательно НЕ в списке: срезает основы
// существительных/прилагательных женского рода («доросла»→«дорос» ≠ «доросл»);
// прошедшее время унифицируется финальными гласными: «читала»/«читали»→«читал».

fn is_cyr(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

/// Стемминговая форма токена (кириллица → основа, прочее — как есть).
pub fn stem_cyr(token: &str) -> String {
    if !token.chars().any(is_cyr) {
        return token.to_string();
    }
    let mut s = token.to_string();
    for _ in 0..2 {
        let n = s.chars().count();
        if n < MIN_WORD {
            break;
        }
        // самый длинной суффикс, после отрезания которого основа ≥ MIN_STEM
        let mut best: &str = "";
        let mut best_len = 0usize;
        for suf in CYR_SUFFIXES {
            let sl = suf.chars().count();
            if sl <= best_len || sl >= n {
                continue;
            }
            if n - sl < MIN_STEM {
                continue;
            }
            if s.ends_with(suf) {
                best = suf;
                best_len = sl;
            }
        }
        if best.is_empty() {
            break;
        }
        s.truncate(s.len() - best.len());
    }
    s
}

/// Токенизация + стемминг: единый путь для индексации И запроса
/// (урок бага «слайд-шоу»: разные токенизаторы → ноль хитов).
pub fn tokenize_stem(s: &str) -> Vec<String> {
    crate::web::extract::web_tokenize(s)
        .into_iter()
        .map(|t| stem_cyr(&t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::extract::web_tokenize;

    #[test]
    fn ukrainian_inflections_unify() {
        // семейство «ініціація» — баг из полевого анализа романа
        let a = stem_cyr("ініціація");
        let b = stem_cyr("ініціації");
        let c = stem_cyr("ініціацію");
        let d = stem_cyr("ініціац");
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
        assert_eq!(a, "ініціац");
    }

    #[test]
    fn adjectives_and_nouns_unify() {
        assert_eq!(stem_cyr("доросла"), stem_cyr("дорослий"));
        assert_eq!(stem_cyr("доросла"), stem_cyr("дорослі"));
        assert_eq!(stem_cyr("книги"), stem_cyr("книгу"));
        assert_eq!(stem_cyr("зміни"), stem_cyr("зміною"));
        assert_eq!(stem_cyr("машина"), stem_cyr("машину"));
    }

    #[test]
    fn latin_untouched() {
        assert_eq!(stem_cyr("ownership"), "ownership");
        assert_eq!(stem_cyr("running"), "running");
    }

    #[test]
    fn short_words_protected() {
        // слова ≤ 4 символов не стеммингуются вовсе
        assert_eq!(stem_cyr("мати"), "мати");
        assert_eq!(stem_cyr("йти"), "йти");
        assert_eq!(stem_cyr("мама"), "мама");
        assert_eq!(stem_cyr("маму"), "маму");
    }

    #[test]
    fn past_tense_not_overstemmed() {
        // «ла» не режется: женские прилагательные/существительные сохраняют основу
        assert_eq!(stem_cyr("доросла"), "доросл");
        // прошедшее время унифицируется финальными гласными
        assert_eq!(stem_cyr("читала"), stem_cyr("читали"));
    }

    #[test]
    fn tokenize_stem_consistent_query_and_doc() {
        let q = tokenize_stem("доросла ініціація");
        let doc = tokenize_stem("дорослі ініціації відбулися");
        // запросные термы обязаны быть в термах документа
        for t in &q {
            assert!(doc.contains(t), "«{t}» нет в {doc:?}");
        }
        assert_eq!(tokenize_stem("Ownership Model"), {
            let raw = web_tokenize("Ownership Model");
            raw.iter().map(|t| stem_cyr(t)).collect::<Vec<_>>()
        });
    }
}
