//! Systematic sentences from a grammar. Every choice the grammar offers is a target: each alternative of each
//! rule, each optional element present and absent, each repetition 0, 1, and 2 times, and, for every rule an
//! alternative uses, each alternative of that rule (pairs). Each target gets the shortest sentence that makes
//! that choice: the cheapest way from the start rule to the choice, and the cheapest expansion everywhere else.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

use serde::Serialize;

use crate::g4::{Atom, Grammar, Suffix};

const UNPRODUCTIVE: u64 = u64::MAX / 4;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Target {
    /// The alternative of a rule.
    Alt { rule: String, alt: usize },
    /// An optional or repeated element, used this many times.
    Count { rule: String, alt: usize, element: usize, count: u32 },
    /// A rule an alternative uses, expanded with this alternative of its own.
    Pair { rule: String, alt: usize, element: usize, child_alt: usize },
}

impl Target {
    pub fn rule(&self) -> &str {
        match self {
            Target::Alt { rule, .. } | Target::Count { rule, .. } | Target::Pair { rule, .. } => rule,
        }
    }

    /// The target whose sentence differs from this one only in this target's choice.
    pub fn baseline(&self, default_alt: usize) -> Target {
        match self {
            Target::Alt { rule, .. } => Target::Alt { rule: rule.clone(), alt: default_alt },
            Target::Count { rule, alt, .. } | Target::Pair { rule, alt, .. } => Target::Alt { rule: rule.clone(), alt: *alt },
        }
    }

    pub fn describe(&self, grammar: &Grammar) -> String {
        let alternative = |rule: &str, alt: usize| {
            let label = grammar.rules.get(rule).and_then(|rule| rule.alts.get(alt)).and_then(|alt| alt.label.clone());
            match label {
                Some(label) => format!("{rule}#{alt} ({label})"),
                None => format!("{rule}#{alt}"),
            }
        };
        match self {
            Target::Alt { rule, alt } => alternative(rule, *alt),
            Target::Count { rule, alt, element, count } => format!("{} element {} ×{}", alternative(rule, *alt), element, count),
            Target::Pair { rule, alt, element, child_alt } => {
                let child = match &grammar.rules[rule].alts[*alt].elements[*element].atom {
                    Atom::Rule(child) => child.clone(),
                    _ => "?".to_string(),
                };
                format!("{} element {} → {}", alternative(rule, *alt), element, alternative(&child, *child_alt))
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Every alternative and every optional or repeated element count.
    Alternatives,
    /// Also every alternative of every rule each alternative uses.
    #[default]
    Pairs,
}

/// One forced choice on the way to a target: in `rule`, take `alt`, and continue the plan through the first use
/// of `element`, used `count` times.
#[derive(Clone, Debug)]
struct Step {
    rule: String,
    alt: usize,
    element: Option<usize>,
    count: u32,
}

pub struct Sentence {
    pub text: String,
    pub target: Target,
    pub covered: BTreeSet<Target>,
}

/// What a grammar's sentences need to be well typed: which rules stand for one value, what type a context
/// wants, and values of each type.
#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Typing {
    /// Rules that stand for one value. Where a sentence does not try a choice inside one, it gets a value of the
    /// type its context wants instead of the rule's shortest derivation.
    #[serde(default)]
    pub value_rules: Vec<String>,
    /// The type values take below a rule (`rule`) or below an alternative taken (`rule#label` or `rule#index`).
    /// `SAME` gives the values below it one type, chosen per use; `ANY` lets them take any type.
    #[serde(default)]
    pub types: BTreeMap<String, String>,
    /// Values of each type, used in turn.
    #[serde(default)]
    pub values: BTreeMap<String, Vec<String>>,
    /// Optional elements present unless a choice says otherwise, as `rule#alt.element`.
    #[serde(default)]
    pub present: Vec<String>,
}

pub struct Deriver<'g> {
    grammar: &'g Grammar,
    start: String,
    separator: String,
    /// Rules and tokens whose text is chosen from a list instead of derived: names, numbers, strings.
    vocabulary: &'g BTreeMap<String, Vec<String>>,
    /// Alternatives to take when nothing forces a choice, so sentences start from a statement that runs.
    prefer: &'g BTreeMap<String, usize>,
    typing: &'g Typing,
    /// Optional elements present by default: rule, alternative, element.
    present: BTreeSet<(String, usize, usize)>,
    cost: HashMap<String, u64>,
    /// The cheapest way into each parser rule: the rule, alternative, and element that uses it.
    via: HashMap<String, (String, usize, usize)>,
    lexer_text: HashMap<String, String>,
}

impl<'g> Deriver<'g> {
    pub fn new(
        grammar: &'g Grammar,
        start: &str,
        separator: &str,
        vocabulary: &'g BTreeMap<String, Vec<String>>,
        prefer: &'g BTreeMap<String, usize>,
        typing: &'g Typing,
    ) -> Deriver<'g> {
        let present = typing
            .present
            .iter()
            .filter_map(|entry| {
                let (rule, position) = entry.split_once('#')?;
                let (alt, element) = position.split_once('.')?;
                Some((rule.to_string(), alt.parse().ok()?, element.parse().ok()?))
            })
            .collect();
        let mut deriver = Deriver {
            grammar,
            start: start.to_string(),
            separator: separator.to_string(),
            vocabulary,
            prefer,
            typing,
            present,
            cost: HashMap::new(),
            via: HashMap::new(),
            lexer_text: HashMap::new(),
        };
        deriver.compute_costs();
        deriver.compute_reach();
        deriver
    }

    fn atom_cost(&self, atom: &Atom) -> u64 {
        match atom {
            Atom::Rule(name) if self.vocabulary.contains_key(name) => 1,
            Atom::Rule(name) => match self.grammar.rules.get(name) {
                // A token counts once in a sentence, however long its text.
                Some(rule) if rule.lexer && !rule.fragment => 1,
                _ => *self.cost.get(name).unwrap_or(&UNPRODUCTIVE),
            },
            Atom::Eof => 0,
            _ => 1,
        }
    }

    fn alt_cost(&self, rule: &str, alt: usize) -> u64 {
        let mut total: u64 = 0;
        for element in &self.grammar.rules[rule].alts[alt].elements {
            let cost = match element.suffix {
                Suffix::Optional | Suffix::Star => 0,
                Suffix::One | Suffix::Plus => self.atom_cost(&element.atom),
            };
            total = total.saturating_add(cost).min(UNPRODUCTIVE);
        }
        total
    }

    /// The fewest tokens each rule can produce, by fixpoint.
    fn compute_costs(&mut self) {
        loop {
            let mut changed = false;
            for (name, rule) in &self.grammar.rules {
                let best = (0..rule.alts.len()).map(|alt| self.alt_cost(name, alt)).min().unwrap_or(UNPRODUCTIVE);
                if best < *self.cost.get(name).unwrap_or(&UNPRODUCTIVE) {
                    self.cost.insert(name.clone(), best);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn cheapest_alt(&self, rule: &str) -> usize {
        let alts = self.grammar.rules[rule].alts.len();
        (0..alts).min_by_key(|alt| self.alt_cost(rule, *alt)).unwrap_or(0)
    }

    fn is_parser_rule(&self, name: &str) -> bool {
        self.grammar.rules.get(name).is_some_and(|rule| !rule.lexer) && !self.vocabulary.contains_key(name)
    }

    /// The cheapest way to reach every parser rule from the start, by Dijkstra over rule uses.
    fn compute_reach(&mut self) {
        let mut distance: HashMap<String, u64> = HashMap::new();
        let mut queue = BinaryHeap::new();
        distance.insert(self.start.clone(), 0);
        queue.push(Reverse((0u64, self.start.clone())));
        while let Some(Reverse((dist, rule))) = queue.pop() {
            if distance.get(&rule).is_some_and(|best| *best < dist) {
                continue;
            }
            let alts = self.grammar.rules[&rule].alts.len();
            for alt in 0..alts {
                let mut total = self.alt_cost(&rule, alt);
                if total >= UNPRODUCTIVE {
                    continue;
                }
                // Paths go through preferred alternatives whenever they can.
                if self.prefer.get(&rule).is_some_and(|preferred| *preferred != alt) {
                    total = total.saturating_add(10_000);
                }
                for (index, element) in self.grammar.rules[&rule].alts[alt].elements.iter().enumerate() {
                    let Atom::Rule(child) = &element.atom else { continue };
                    if !self.is_parser_rule(child) {
                        continue;
                    }
                    let own = match element.suffix {
                        Suffix::Optional | Suffix::Star => 0,
                        _ => self.atom_cost(&element.atom),
                    };
                    let candidate = dist.saturating_add(total - own);
                    if candidate < *distance.get(child).unwrap_or(&UNPRODUCTIVE) {
                        distance.insert(child.clone(), candidate);
                        self.via.insert(child.clone(), (rule.clone(), alt, index));
                        queue.push(Reverse((candidate, child.clone())));
                    }
                }
            }
        }
    }

    pub fn reachable(&self, rule: &str) -> bool {
        rule == self.start || self.via.contains_key(rule)
    }

    /// Every target of every reachable parser rule.
    pub fn targets(&self, coverage: Coverage) -> Vec<Target> {
        let mut targets = Vec::new();
        for (name, rule) in &self.grammar.rules {
            if rule.lexer || !self.is_parser_rule(name) || !self.reachable(name) {
                continue;
            }
            for (alt, alternative) in rule.alts.iter().enumerate() {
                if self.alt_cost(name, alt) >= UNPRODUCTIVE {
                    continue;
                }
                targets.push(Target::Alt { rule: name.clone(), alt });
                for (index, element) in alternative.elements.iter().enumerate() {
                    let counts: &[u32] = match element.suffix {
                        Suffix::One => &[],
                        Suffix::Optional => &[0, 1],
                        Suffix::Star => &[0, 1, 2],
                        Suffix::Plus => &[1, 2],
                    };
                    if self.atom_cost(&element.atom) >= UNPRODUCTIVE {
                        continue;
                    }
                    for count in counts {
                        targets.push(Target::Count { rule: name.clone(), alt, element: index, count: *count });
                    }
                    if coverage == Coverage::Pairs {
                        if let Atom::Rule(child) = &element.atom {
                            if self.is_parser_rule(child) {
                                for child_alt in 0..self.grammar.rules[child].alts.len() {
                                    if self.alt_cost(child, child_alt) < UNPRODUCTIVE {
                                        targets.push(Target::Pair { rule: name.clone(), alt, element: index, child_alt });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        targets
    }

    /// The forced choices from the start rule into `rule`.
    fn path_to(&self, rule: &str) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut current = rule.to_string();
        while current != self.start {
            let (parent, alt, element) = self.via[&current].clone();
            steps.push(Step { rule: parent.clone(), alt, element: Some(element), count: 1 });
            current = parent;
        }
        steps.reverse();
        steps
    }

    /// The alternative taken when nothing forces one: the preferred one, else the cheapest.
    pub fn default_alt(&self, rule: &str) -> usize {
        match self.prefer.get(rule) {
            Some(alt) if *alt < self.grammar.rules[rule].alts.len() && self.alt_cost(rule, *alt) < UNPRODUCTIVE => *alt,
            _ => self.cheapest_alt(rule),
        }
    }

    /// Joins tokens; `auto` puts a space only where two tokens would otherwise run together.
    fn join(&self, tokens: &[String]) -> String {
        if self.separator != "auto" {
            return tokens.join(&self.separator);
        }
        let mut text = String::new();
        for token in tokens {
            let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
            if let (Some(last), Some(first)) = (text.chars().last(), token.chars().next()) {
                if word(last) && word(first) {
                    text.push(' ');
                }
            }
            text.push_str(token);
        }
        text
    }

    /// The shortest sentence that makes the target's choice.
    pub fn sentence(&self, target: &Target) -> Sentence {
        let mut plan = self.path_to(target.rule());
        match target {
            Target::Alt { rule, alt } => plan.push(Step { rule: rule.clone(), alt: *alt, element: None, count: 0 }),
            Target::Count { rule, alt, element, count } => plan.push(Step { rule: rule.clone(), alt: *alt, element: Some(*element), count: *count }),
            Target::Pair { rule, alt, element, child_alt } => {
                plan.push(Step { rule: rule.clone(), alt: *alt, element: Some(*element), count: 1 });
                let Atom::Rule(child) = &self.grammar.rules[rule].alts[*alt].elements[*element].atom else { unreachable!() };
                plan.push(Step { rule: child.clone(), alt: *child_alt, element: None, count: 0 });
            }
        }
        let mut tokens = Vec::new();
        let mut covered = BTreeSet::new();
        let mut occurrences: HashMap<String, usize> = HashMap::new();
        let mut state = Typed { context: None, uses: HashMap::new(), same: 0 };
        self.expand(&self.start.clone(), &plan, &mut tokens, &mut covered, &mut occurrences, 0, &mut state);
        Sentence { text: self.join(&tokens).trim().to_string(), target: target.clone(), covered }
    }

    /// Expands a rule, following the plan when it names this rule; returns the alternative taken.
    fn expand(
        &self,
        rule: &str,
        plan: &[Step],
        tokens: &mut Vec<String>,
        covered: &mut BTreeSet<Target>,
        occurrences: &mut HashMap<String, usize>,
        depth: usize,
        state: &mut Typed,
    ) -> Option<usize> {
        if let Some(values) = self.vocabulary.get(rule) {
            // Each occurrence takes the next value, so names repeat the way variables and their uses do.
            let seen = occurrences.entry(rule.to_string()).or_default();
            tokens.push(values[*seen % values.len()].clone());
            *seen += 1;
            return None;
        }
        let definition = &self.grammar.rules[rule];
        if definition.lexer {
            tokens.push(self.token_text(rule));
            return None;
        }
        let forced = plan.first().is_some_and(|step| step.rule == rule);
        // A value nothing here is trying a choice in: a value of the type the context wants.
        if !forced && self.typing.value_rules.iter().any(|name| name == rule) {
            let ty = self.typing.types.get(rule).cloned().or_else(|| state.context.clone()).unwrap_or_else(|| "INT64".to_string());
            let ty = state.resolve(&ty, &self.typing);
            if let Some(values) = self.typing.values.get(&ty) {
                let used = state.uses.entry(ty.clone()).or_default();
                tokens.push(values[*used % values.len()].clone());
                *used += 1;
                return None;
            }
        }
        let (alt, step) = match plan.first() {
            Some(step) if step.rule == rule => (step.alt, Some(step)),
            // Deep down, a preferred alternative could recurse forever; the cheapest one cannot.
            _ if depth > 40 => (self.cheapest_alt(rule), None),
            _ => (self.default_alt(rule), None),
        };
        covered.insert(Target::Alt { rule: rule.to_string(), alt });
        // The type below this alternative: its own, its rule's, or the context's.
        let label = definition.alts[alt].label.clone().unwrap_or_default();
        let outer = state.context.clone();
        let declared = self
            .typing
            .types
            .get(&format!("{rule}#{label}"))
            .or_else(|| self.typing.types.get(&format!("{rule}#{alt}")))
            .or_else(|| self.typing.types.get(rule))
            .cloned();
        // An alternative such as `A (OR A)*` passes a value through when it repeats nothing, and then its type
        // does not apply.
        let elements = &definition.alts[alt].elements;
        let optional = elements.iter().enumerate().filter(|(_, element)| element.suffix != Suffix::One).map(|(index, _)| index).collect::<Vec<_>>();
        let operator_used = optional.is_empty()
            || optional.iter().any(|index| {
                step.is_some_and(|step| step.element == Some(*index) && step.count > 0) || self.present.contains(&(rule.to_string(), alt, *index))
            });
        if let (Some(declared), true) = (declared, operator_used) {
            state.context = Some(state.resolve(&declared, &self.typing));
        }
        for (index, element) in definition.alts[alt].elements.iter().enumerate() {
            let continues = step.is_some_and(|step| step.element == Some(index));
            let count = if continues {
                step.unwrap().count.max(if element.suffix == Suffix::One || element.suffix == Suffix::Plus { 1 } else { 0 })
            } else {
                match element.suffix {
                    Suffix::One | Suffix::Plus => 1,
                    Suffix::Optional | Suffix::Star => {
                        if self.present.contains(&(rule.to_string(), alt, index)) { 1 } else { 0 }
                    }
                }
            };
            if element.suffix != Suffix::One {
                covered.insert(Target::Count { rule: rule.to_string(), alt, element: index, count });
            }
            for occurrence in 0..count {
                let rest: &[Step] = if continues && occurrence == 0 { &plan[1..] } else { &[] };
                match &element.atom {
                    Atom::Rule(child) => {
                        if let Some(child_alt) = self.expand(child, rest, tokens, covered, occurrences, depth + 1, state) {
                            if self.is_parser_rule(child) {
                                covered.insert(Target::Pair { rule: rule.to_string(), alt, element: index, child_alt });
                            }
                        }
                    }
                    Atom::Literal(text) => tokens.push(text.clone()),
                    Atom::Eof => {}
                    other => tokens.push(self.char_of(other).to_string()),
                }
            }
        }
        state.context = outer;
        Some(alt)
    }

    /// A rule's alternatives and their elements, numbered as `present` and `types` keys use them.
    pub fn describe_rule(&self, rule: &str) -> String {
        let Some(definition) = self.grammar.rules.get(rule) else { return format!("no rule {rule}") };
        let mut text = String::new();
        for (alt, alternative) in definition.alts.iter().enumerate() {
            let label = alternative.label.as_deref().map(|label| format!(" #{label}")).unwrap_or_default();
            text.push_str(&format!("{rule}#{alt}{label}:\n"));
            for (index, element) in alternative.elements.iter().enumerate() {
                let atom = match &element.atom {
                    Atom::Rule(name) => match self.grammar.rules.get(name) {
                        Some(lifted) if lifted.parent.is_some() => {
                            let inner: Vec<String> = lifted.alts.iter().map(|alt| {
                                alt.elements.iter().map(|element| match &element.atom {
                                    Atom::Rule(name) => name.clone(),
                                    Atom::Literal(text) => format!("'{text}'"),
                                    _ => "…".to_string(),
                                }).collect::<Vec<_>>().join(" ")
                            }).collect();
                            format!("{name} = ( {} )", inner.join(" | "))
                        }
                        _ => name.clone(),
                    },
                    Atom::Literal(text) => format!("'{text}'"),
                    other => format!("{other:?}"),
                };
                text.push_str(&format!("  .{index} {atom} {:?}\n", element.suffix));
            }
        }
        text
    }

    /// A character an atom of a lexer rule matches.
    fn char_of(&self, atom: &Atom) -> char {
        match atom {
            Atom::Set { ranges, negated: false } => {
                // Prefer a plain ASCII letter or digit from the set.
                for (low, high) in ranges {
                    for c in ['a', 'x', '0', '1', 'A'] {
                        if *low <= c && c <= *high {
                            return c;
                        }
                    }
                }
                ranges.first().map(|(low, _)| *low).unwrap_or('a')
            }
            Atom::Set { ranges, negated: true } => {
                ['a', 'x', '0', 'Z', '_'].into_iter().find(|c| !ranges.iter().any(|(low, high)| low <= c && c <= high)).unwrap_or('a')
            }
            Atom::NotLiterals(literals) => ['a', 'x'].into_iter().find(|c| !literals.iter().any(|literal| literal.starts_with(*c))).unwrap_or('b'),
            _ => 'a',
        }
    }

    /// The shortest text of a token.
    fn token_text(&self, rule: &str) -> String {
        if let Some(values) = self.vocabulary.get(rule) {
            return values[0].clone();
        }
        if let Some(text) = self.lexer_text.get(rule) {
            return text.clone();
        }
        let mut text = String::new();
        self.lex(rule, &mut text, 0);
        text
    }

    fn lex(&self, rule: &str, text: &mut String, depth: usize) {
        if depth > 64 {
            return;
        }
        if let Some(values) = self.vocabulary.get(rule) {
            text.push_str(&values[0]);
            return;
        }
        let definition = &self.grammar.rules[rule];
        let alt = self.cheapest_alt(rule);
        for element in &definition.alts[alt].elements {
            let count = match element.suffix {
                Suffix::One | Suffix::Plus => 1,
                Suffix::Optional | Suffix::Star => 0,
            };
            for _ in 0..count {
                match &element.atom {
                    Atom::Rule(child) => self.lex(child, text, depth + 1),
                    Atom::Literal(literal) => text.push_str(literal),
                    Atom::Eof => {}
                    other => text.push(self.char_of(other)),
                }
            }
        }
    }
}

/// The typing state of one sentence's derivation.
struct Typed {
    /// The type values take here.
    context: Option<String>,
    /// How many values of each type the sentence has used, so the values alternate.
    uses: HashMap<String, usize>,
    /// How many SAME contexts the sentence has resolved, so they take different types in turn.
    same: usize,
}

impl Typed {
    /// A concrete type for a declared one: SAME and ANY take the next type that has values.
    fn resolve(&mut self, declared: &str, typing: &Typing) -> String {
        if declared != "SAME" && declared != "ANY" {
            return declared.to_string();
        }
        let types: Vec<&String> = typing.values.keys().collect();
        if types.is_empty() {
            return "INT64".to_string();
        }
        self.same += 1;
        types[(self.same - 1) % types.len()].clone()
    }
}
