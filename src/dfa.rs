use std::{
    collections::BTreeSet,
    io::Write, cmp::Ordering, rc::Rc,
    hash::Hash,
};
use hashbrown::{HashMap, HashSet};

use roaring::{MultiOps, RoaringBitmap};
use ustr::{Ustr, ustr};

use crate::{regex::{Position, AugmentedRegex, Input}, grammar::{Specialization, DFARef}};
use crate::StateId;


// Every state in a DFA is formally defined to have a transition on *every* input symbol.  In
// our applications that's not the case, so we add artificial transitions to a special, designated
// "dead" state.  Otherwise the minimization algorithm doesn't work as intended.
pub const DEAD_STATE_ID: StateId = 0;
pub const FIRST_STATE_ID: StateId = 1;


#[derive(Debug, Clone)]
pub struct DFA {
    pub starting_state: StateId,
    pub transitions: HashMap<StateId, HashMap<Input, StateId>>,
    pub accepting_states: RoaringBitmap,
    pub input_symbols: Rc<HashSet<Input>>,
}


impl std::hash::Hash for DFA {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.starting_state.hash(state);

        for (from, tos) in &self.transitions {
            for (input, to) in tos {
                (from, input, to).hash(state);
            }
        }

        for elem in &self.accepting_states {
            elem.hash(state);
        }

        for elem in self.input_symbols.iter() {
            elem.hash(state);
        }
    }
}


// Reference:
//  * The Dragon Book: 3.9.5 Converting a Regular Expression Directly to a DFA
fn dfa_from_regex(regex: &AugmentedRegex) -> DFA {
    let mut unallocated_state_id = FIRST_STATE_ID;
    let combined_starting_state: BTreeSet<Position> = regex.firstpos();
    let combined_starting_state_id = unallocated_state_id;
    unallocated_state_id += 1;

    let mut dstates: HashMap<BTreeSet<Position>, StateId> = HashMap::from_iter([(combined_starting_state.clone(), combined_starting_state_id)]);

    let followpos = regex.followpos();

    let mut dtran: HashMap<StateId, HashMap<Input, StateId>> = Default::default();
    let mut unmarked_states: HashSet<BTreeSet<Position>> = Default::default();
    unmarked_states.insert(combined_starting_state.clone());
    loop {
        let combined_state = match unmarked_states.iter().next() {
            Some(state) => state.clone(),
            None => break,
        };
        unmarked_states.remove(&combined_state);
        let from_combined_state_id = *dstates.get(&combined_state).unwrap();
        let from_entry = dtran.entry(from_combined_state_id).or_default();
        for input in regex.input_symbols.iter() {
            let mut u = RoaringBitmap::new();
            for pos in &combined_state {
                let pos_usize = usize::try_from(*pos).unwrap();
                if regex.input_from_position.get(pos_usize) == Some(input) {
                    if let Some(positions) = followpos.get(&pos) {
                        u |= positions;
                    }
                }
            }
            if !u.is_empty() {
                let u = BTreeSet::from_iter(u);
                if !dstates.contains_key(&u) {
                    dstates.insert(u.clone(), unallocated_state_id);
                    unallocated_state_id += 1;
                    unmarked_states.insert(u.clone());
                }
                let to_combined_state_id = dstates.get(&u).unwrap();
                from_entry.insert(input.clone(), *to_combined_state_id);
            }
        }
    }

    // The accepting states are those containing the position for the endmarker symbol #.
    let accepting_states: RoaringBitmap = {
        let mut accepting_states = RoaringBitmap::default();
        for (combined_state, state_id) in &dstates {
            if combined_state.contains(&regex.endmarker_position) {
                accepting_states.insert((*state_id).into());
            }
        }
        accepting_states
    };

    DFA {
        starting_state: *dstates.get(&combined_starting_state).unwrap(),
        transitions: dtran,
        accepting_states,
        input_symbols: Rc::clone(&regex.input_symbols),
    }
}


struct HashableRoaringBitmap(Rc<RoaringBitmap>);


impl PartialEq for HashableRoaringBitmap {
    fn eq(&self, other: &Self) -> bool {
        if self.0.len() != other.0.len() {
            return false;
        }
        self.0.iter().zip(other.0.iter()).all(|(left, right)| left == right)
    }
}

impl Eq for HashableRoaringBitmap {
}


impl std::hash::Hash for HashableRoaringBitmap {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for elem in self.0.iter() {
            elem.hash(state);
        }
    }
}


#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
struct SetInternId(usize);


#[derive(Default)]
struct SetInternPool {
    pool: Vec<Rc<RoaringBitmap>>,
    id_from_set: HashMap<HashableRoaringBitmap, usize>,
}

impl SetInternPool {
    fn intern(&mut self, set: RoaringBitmap) -> SetInternId {
        let rc = Rc::new(set);
        let id = *self.id_from_set.entry(HashableRoaringBitmap(Rc::clone(&rc))).or_insert_with(|| {
            let id = self.pool.len();
            self.pool.push(rc);
            id
        });
        SetInternId(id)
    }

    fn get(&self, id: SetInternId) -> Option<Rc<RoaringBitmap>> {
        self.pool.get(id.0).cloned()
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
struct Transition {
    from: StateId,
    to: StateId,
    input: Input,
}


fn keep_only_states_with_input_transitions(starting_state: StateId, transitions: &[Transition], accepting_states: &RoaringBitmap) -> (Vec<Transition>, RoaringBitmap) {
    let states_with_input_transition = RoaringBitmap::from_iter(transitions.iter().map(|transition| u32::from(transition.to)));

    let alive_accepting_states = RoaringBitmap::from_sorted_iter(accepting_states.iter().filter(|state| *state == starting_state.into() || states_with_input_transition.contains(*state))).unwrap();

    let alive_transitions: Vec<Transition> = transitions.iter().filter(|transition| {
        if transition.from == starting_state {
            return true;
        }
        if !states_with_input_transition.contains(transition.from.into()) || !states_with_input_transition.contains(transition.to.into()) {
            return false;
        }
        true
    }).cloned().collect();

    (alive_transitions, alive_accepting_states)
}

fn eliminate_nonaccepting_states_without_output_transitions(transitions: &[Transition], accepting_states: &RoaringBitmap) -> Vec<Transition> {
    let states_with_output_transition = RoaringBitmap::from_iter(transitions.iter().map(|transition| u32::from(transition.from)));
    let alive_transitions: Vec<Transition> = transitions.iter().filter(|transition| accepting_states.contains(transition.to.into()) || states_with_output_transition.contains(transition.to.into())).cloned().collect();
    alive_transitions
}

fn renumber_states(starting_state: StateId, transitions: &[Transition], accepting_states: &RoaringBitmap) -> (StateId, Vec<Transition>, RoaringBitmap) {
    let new_from_old_state_id = {
        let mut new_from_old_state_id: HashMap<StateId, StateId> = Default::default();
        let mut unallocated_state_id = 0;

        new_from_old_state_id.entry(starting_state).or_insert_with(|| {
            let result = unallocated_state_id;
            unallocated_state_id += 1;
            result
        });

        for transition in transitions {
            new_from_old_state_id.entry(transition.from).or_insert_with(|| {
                let result = unallocated_state_id;
                unallocated_state_id += 1;
                result
            });
            new_from_old_state_id.entry(transition.to).or_insert_with(|| {
                let result = unallocated_state_id;
                unallocated_state_id += 1;
                result
            });
        }
        new_from_old_state_id
    };

    let new_starting_state = *new_from_old_state_id.get(&starting_state).unwrap();

    let new_transitions: Vec<Transition> = transitions.iter().map(|Transition { from, to, input }| Transition {
        from: *new_from_old_state_id.get(from).unwrap(),
        to: *new_from_old_state_id.get(to).unwrap(),
        input: input.clone(),
    }).collect();

    let new_accepting_states: RoaringBitmap = RoaringBitmap::from_iter(accepting_states.iter().map(|old| u32::from(*new_from_old_state_id.get(&u16::try_from(old).unwrap()).unwrap())));

    (new_starting_state, new_transitions, new_accepting_states)
}

fn hashmap_transitions_from_vec(transitions: &[Transition]) -> HashMap<StateId, HashMap<Input, StateId>> {
    let mut result: HashMap<StateId, HashMap<Input, StateId>> = Default::default();

    for transition in transitions {
        result.entry(transition.from).or_default().insert(transition.input.clone(), transition.to);
    }

    result
}

fn make_inverse_transitions_lookup_table(transitions: &HashMap<StateId, HashMap<Input, StateId>>, input_symbols: Rc<HashSet<Input>>) -> Vec<Transition> {
    let mut result: Vec<Transition> = Default::default();
    for (from, tos) in transitions {
        let meaningful_inputs: HashSet<Input> = tos.keys().cloned().collect();
        for (input, to) in tos {
            result.push(Transition {
                from: *from,
                to: *to,
                input: input.clone(),
            });
        }
        for input in input_symbols.difference(&meaningful_inputs) {
            result.push(Transition {
                from: *from,
                to: DEAD_STATE_ID,
                input: input.clone(),
            });
        }
    }

    result.sort_unstable_by_key(|transition| transition.to);
    result.dedup(); // should be redundant
    result
}

// Hopcroft's DFA minimization algorithm.
// References:
//  * The Dragon Book: Minimizing the Number of states of a DFA
//  * Engineering a Compiler, 3rd ed, 2.4.4 DFA to Minimal DFA
//  * https://github.com/BurntSushi/regex-automata/blob/c61a6d0f19b013dc832755375709023dfb9d5a8f/src/dfa/minimize.rs#L87
//
// TODO Use https://docs.rs/nohash-hasher/ for Hash{Map,Set}<StateId, ...>?
fn do_minimize(dfa: &DFA) -> DFA {
    let mut pool = SetInternPool::default();
    let mut partition: HashSet<SetInternId> = {
        let dead_state_group = RoaringBitmap::from_iter([u32::from(DEAD_STATE_ID)]);
        let all_states = dfa.get_all_states();
        let nonaccepting_states = [&all_states, &dfa.accepting_states, &dead_state_group].difference();
        if nonaccepting_states.is_empty() {
            // Nothing to minimize
            return dfa.clone();
        }
        let nonaccepting_states_intern_id = pool.intern(nonaccepting_states);
        let accepting_states_intern_id = pool.intern(dfa.accepting_states.clone());
        let dead_state_intern_id = pool.intern(dead_state_group);
        HashSet::from_iter([dead_state_intern_id, accepting_states_intern_id, nonaccepting_states_intern_id])
    };
    let mut worklist = partition.clone();
    let inverse_transitions = make_inverse_transitions_lookup_table(&dfa.transitions, Rc::clone(&dfa.input_symbols));
    loop {
        let group_id = match worklist.iter().next() {
            Some(group_id) => *group_id,
            None => break,
        };
        worklist.remove(&group_id);
        let group = pool.get(group_id).unwrap(); // group is a better name for 's' from the book
        let group_min = group.min().unwrap();
        let group_max = group.max().unwrap();
        let inbetween_index = match inverse_transitions.binary_search_by(|transition| {
            let to: u32 = transition.to.into();
            if to < group_min {
                return Ordering::Less;
            }
            if to > group_max {
                return Ordering::Greater;
            }
            Ordering::Equal
        }) {
            Ok(index) => index,
            Err(_) => continue,
        };
        let lower_bound = {
            let mut lower_bound = inbetween_index;
            while lower_bound > 0 && u32::from(inverse_transitions[lower_bound-1].to) >= group_min {
                lower_bound -= 1;
            }
            lower_bound
        };
        let upper_bound = {
            let mut upper_bound = inbetween_index;
            while upper_bound < inverse_transitions.len() - 1 && u32::from(inverse_transitions[upper_bound+1].to) <= group_max {
                upper_bound += 1;
            }
            upper_bound
        };

        let group_transitions: Vec<Transition> = inverse_transitions[lower_bound..=upper_bound].iter().filter(|transition| group.contains(transition.to.into())).cloned().collect();
        for input in dfa.input_symbols.iter() {
            let from_states: RoaringBitmap = group_transitions.iter().filter(|transition| transition.input == *input).map(|transition| u32::from(transition.from)).collect();
            let overlapping_sets: Vec<SetInternId> = partition.iter().filter(|set_id| !pool.get(**set_id).unwrap().is_disjoint(&from_states)).copied().collect();
            for intern_id in overlapping_sets {
                let states = pool.get(intern_id).unwrap();
                let states_to_remove = [&states, &from_states].intersection();
                let remaining_states = [&states, &states_to_remove].difference();
                if remaining_states.is_empty() {
                    continue;
                }

                let num_states_to_remove = states_to_remove.len();
                let num_remaining_states = remaining_states.len();

                partition.remove(&intern_id);
                let states_to_remove_intern_id = pool.intern(states_to_remove);
                let remaining_states_intern_id = pool.intern(remaining_states);
                partition.insert(states_to_remove_intern_id);
                partition.insert(remaining_states_intern_id);

                if worklist.contains(&intern_id) {
                    worklist.remove(&intern_id);
                    worklist.insert(states_to_remove_intern_id);
                    worklist.insert(remaining_states_intern_id);
                }
                else if num_states_to_remove <= num_remaining_states {
                    worklist.insert(states_to_remove_intern_id);
                }
                else {
                    worklist.insert(remaining_states_intern_id);
                }
                if group_id == intern_id {
                    break;
                }
            }
        }
    }

    let representative_id_from_state_id = {
        let mut representative_id_from_state_id: HashMap<StateId, StateId> = Default::default();
        for intern_id in &partition {
            let partition_element = pool.get(*intern_id).unwrap();
            let representative_state_id = partition_element.min().unwrap();
            for state_id in partition_element.iter() {
                representative_id_from_state_id.insert(StateId::try_from(state_id).unwrap(), StateId::try_from(representative_state_id).unwrap());
            }
        }
        representative_id_from_state_id
    };

    let starting_state = *representative_id_from_state_id.get(&dfa.starting_state).unwrap();

    let accepting_states = {
        let mut accepting_states: RoaringBitmap = Default::default();
        for state_id in &dfa.accepting_states {
            accepting_states.insert((*representative_id_from_state_id.get(&u16::try_from(state_id).unwrap()).unwrap()).into());
        }
        accepting_states
    };


    let transitions = {
        let mut transitions: Vec<Transition> = Default::default();
        for (from, tos) in &dfa.transitions {
            for (input, to) in tos {
                let representative = representative_id_from_state_id.get(to).unwrap();
                transitions.push(Transition { from: *from, to: *representative, input: input.clone() });
            }
        }
        transitions
    };

    let (transitions, accepting_states) = keep_only_states_with_input_transitions(starting_state, &transitions, &accepting_states);
    let transitions = eliminate_nonaccepting_states_without_output_transitions(&transitions, &accepting_states);
    let (starting_state, transitions, accepting_states) = renumber_states(starting_state, &transitions, &accepting_states);
    let transitions = hashmap_transitions_from_vec(&transitions);
    DFA {
        starting_state,
        transitions,
        accepting_states,
        input_symbols: Rc::clone(&dfa.input_symbols),
    }
}


fn do_to_dot<W: Write>(output: &mut W, dfa: &DFA, identifiers_prefix: &str, recursion_level: usize) -> std::result::Result<(), std::io::Error> {
    let indentation = format!("\t{}", str::repeat("\t", recursion_level));

    let id_from_dfa = dfa.get_subwords(0);

    if dfa.accepting_states.contains(dfa.starting_state.into()) {
        writeln!(output, "{indentation}node [shape = doubleoctagon];")?;
    }
    else {
        writeln!(output, "{indentation}node [shape = octagon];")?;
    }
    writeln!(output, "{indentation}_{identifiers_prefix}{}[label=\"{identifiers_prefix}{}\"];", dfa.starting_state, dfa.starting_state)?;

    let regular_states = {
        let mut states = [&dfa.get_all_states(), &dfa.accepting_states].difference();
        states.remove(dfa.starting_state.into());
        states
    };

    writeln!(output, "{indentation}node [shape = circle];")?;
    for state in regular_states {
        writeln!(output, "{indentation}_{identifiers_prefix}{}[label=\"{identifiers_prefix}{}\"];", state, state)?;
    }

    write!(output, "\n")?;

    writeln!(output, "{indentation}node [shape = doublecircle];")?;
    for state in &dfa.accepting_states {
        writeln!(output, "{indentation}_{identifiers_prefix}{}[label=\"{identifiers_prefix}{}\"];", state, state)?;
    }

    write!(output, "\n")?;

    for (subdfa, subdfa_id) in &id_from_dfa {
        writeln!(output, "{indentation}subgraph cluster_{identifiers_prefix}{subdfa_id} {{")?;
        writeln!(output, "{indentation}\tlabel=\"subword {subdfa_id}\";")?;
        writeln!(output, "{indentation}\tcolor=grey91;")?;
        writeln!(output, "{indentation}\tstyle=filled;")?;
        let subdfa_identifiers_prefix = &format!("{subdfa_id}_");
        do_to_dot(output, subdfa.as_ref(), &subdfa_identifiers_prefix, recursion_level + 1)?;
        writeln!(output, "{indentation}}}")?;
    }

    for (from, tos) in &dfa.transitions {
        for (input, to) in tos {
            match input {
                Input::Literal(_, _) | Input::Nonterminal(_, _) | Input::Command(_) => {
                    let label = format!("{}", input).replace("\"", "\\\"");
                    writeln!(output, "{indentation}_{identifiers_prefix}{} -> _{identifiers_prefix}{} [label=\"{}\"];", from, to, label)?;
                }
                Input::Subword(subdfa) => {
                    let subdfa_id = *id_from_dfa.get(subdfa).unwrap();
                    let subdfa_identifiers_prefix = &format!("{subdfa_id}_");
                    writeln!(output, r#"{indentation}_{identifiers_prefix}{} -> _{subdfa_identifiers_prefix}{} [style="dashed"];"#, from, subdfa.as_ref().starting_state)?;
                    for subdfa_accepting_state in &subdfa.as_ref().accepting_states {
                        writeln!(output, r#"{indentation}_{subdfa_identifiers_prefix}{} -> _{identifiers_prefix}{} [style="dashed"];"#, subdfa_accepting_state, to)?;
                    }
                },
            }
        }
    }

    Ok(())
}

fn do_get_subdfa_command_transitions(dfa: &DFA, result: &mut Vec<(StateId, Ustr)>) {
    for (from, tos) in &dfa.transitions {
        for (input, _) in tos {
            let cmd = match input {
                Input::Command(cmd) => *cmd,
                Input::Subword(..) => unreachable!(),
                Input::Nonterminal(..) => continue,
                Input::Literal(..) => continue,
            };
            result.push((*from, cmd));
        }
    }
}


fn do_get_subdfa_bash_command_transitions(dfa: &DFA, result: &mut Vec<(StateId, Ustr)>) {
    for (from, input) in dfa.iter_inputs() {
        match input {
            Input::Nonterminal(_, Some(Specialization { bash: Some(cmd), .. })) => result.push((from, cmd)),
            Input::Nonterminal(..) => continue,
            Input::Subword(..) => unreachable!(),
            Input::Command(..) => continue,
            Input::Literal(..) => continue,
        };
    }
}


fn do_get_subdfa_fish_command_transitions(dfa: &DFA, result: &mut Vec<(StateId, Ustr)>) {
    for (from, input) in dfa.iter_inputs() {
        match input {
            Input::Nonterminal(_, Some(Specialization { fish: Some(cmd), .. })) => result.push((from, cmd)),
            Input::Nonterminal(..) => continue,
            Input::Subword(..) => unreachable!(),
            Input::Command(..) => continue,
            Input::Literal(..) => continue,
        };
    }
}

fn do_get_subdfa_zsh_command_transitions(dfa: &DFA, result: &mut Vec<(StateId, Ustr)>) {
    for (from, input) in dfa.iter_inputs() {
        match input {
            Input::Nonterminal(_, Some(Specialization { zsh: Some(cmd), .. })) => result.push((from, cmd)),
            Input::Nonterminal(..) => continue,
            Input::Subword(..) => unreachable!(),
            Input::Command(..) => continue,
            Input::Literal(..) => continue,
        };
    }
}


impl DFA {
    pub fn from_regex(regex: &AugmentedRegex) -> Self {
        dfa_from_regex(regex)
    }

    pub fn minimize(&self) -> Self {
        do_minimize(self)
    }

    pub fn iter_inputs(&self) -> impl Iterator<Item=(StateId, Input)> + '_ {
        self.transitions.iter().map(|(from, tos)| tos.iter().map(|(input, _)| (*from, input.clone()))).flatten()
    }

    pub fn iter_transitions_from(&self, from: StateId) -> impl Iterator<Item=(Input, StateId)> {
        match self.transitions.get(&from) {
            Some(transitions) => transitions.clone().into_iter(),
            None => HashMap::<Input, StateId>::default().into_iter(),
        }
    }

    pub fn accepts_str(&self, mut input: &str) -> bool {
        let mut current_state = self.starting_state;
        'outer: while !input.is_empty() {
            for (transition_input, to) in self.iter_transitions_from(current_state) {
                if let Input::Literal(ustr, _) = transition_input {
                    let s = ustr.as_str();
                    if input.starts_with(s) {
                        input = &input[s.len()..];
                        current_state = to;
                        continue 'outer;
                    }
                }
            }

            for (transition_input, to) in self.iter_transitions_from(current_state) {
                if transition_input.matches_anything() {
                    current_state = to;
                    break 'outer;
                }
            }

            break;
        }
        self.accepting_states.contains(current_state.into())
    }

    pub fn get_all_literals(&self) -> Vec<(Ustr, Option<Ustr>)> {
        self.input_symbols.iter().filter_map(|input| match input {
            Input::Literal(input, description) => Some((*input, *description)),
            Input::Subword(..) => None,
            Input::Nonterminal(..) => None,
            Input::Command(..) => None,
        }).collect()
    }

    pub fn get_command_transitions(&self) -> (Vec<(StateId, Ustr)>, HashMap<DFARef, Vec<(StateId, Ustr)>>) {
        let mut top_level: Vec<(StateId, Ustr)> = Default::default();
        let mut subdfas: HashMap<DFARef, Vec<(StateId, Ustr)>> = Default::default();
        for (from, tos) in &self.transitions {
            for (input, _) in tos {
                let cmd = match input {
                    Input::Command(cmd) => *cmd,
                    Input::Subword(subdfa) => {
                        if subdfas.contains_key(subdfa) {
                            continue;
                        }
                        let mut transitions = subdfas.entry(subdfa.clone()).or_default();
                        do_get_subdfa_command_transitions(subdfa.as_ref(), &mut transitions);
                        continue;
                    },
                    Input::Nonterminal(..) => continue,
                    Input::Literal(..) => continue,
                };
                top_level.push((*from, cmd));
            }
        }
        (top_level, subdfas)
    }

    pub fn get_bash_command_transitions(&self) -> (Vec<(StateId, Ustr)>, HashMap<DFARef, Vec<(StateId, Ustr)>>) {
        let mut top_level: Vec<(StateId, Ustr)> = Default::default();
        let mut subdfas: HashMap<DFARef, Vec<(StateId, Ustr)>> = Default::default();
        for (from, input) in self.iter_inputs() {
            match input {
                Input::Nonterminal(_, Some(Specialization { bash: Some(cmd), .. })) => top_level.push((from, cmd)),
                Input::Subword(subdfa) => {
                    if subdfas.contains_key(&subdfa) {
                        continue;
                    }
                    let mut transitions = subdfas.entry(subdfa.clone()).or_default();
                    do_get_subdfa_bash_command_transitions(subdfa.as_ref(), &mut transitions);
                    continue;
                },
                Input::Nonterminal(..) | Input::Command(..) | Input::Literal(..) => {},
            };
        }
        (top_level, subdfas)
    }

    pub fn get_fish_command_transitions(&self) -> (Vec<(StateId, Ustr)>, HashMap<DFARef, Vec<(StateId, Ustr)>>) {
        let mut top_level: Vec<(StateId, Ustr)> = Default::default();
        let mut subdfas: HashMap<DFARef, Vec<(StateId, Ustr)>> = Default::default();
        for (from, input) in self.iter_inputs() {
            match input {
                Input::Nonterminal(_, Some(Specialization { fish: Some(cmd), .. })) => top_level.push((from, cmd)),
                Input::Subword(subdfa) => {
                    if subdfas.contains_key(&subdfa) {
                        continue;
                    }
                    let mut transitions = subdfas.entry(subdfa.clone()).or_default();
                    do_get_subdfa_fish_command_transitions(subdfa.as_ref(), &mut transitions);
                    continue;
                },
                Input::Nonterminal(..) | Input::Command(..) | Input::Literal(..) => {},
            }
        }
        (top_level, subdfas)
    }

    pub fn get_zsh_command_transitions(&self) -> (Vec<(StateId, Ustr)>, HashMap<DFARef, Vec<(StateId, Ustr)>>) {
        let mut top_level: Vec<(StateId, Ustr)> = Default::default();
        let mut subdfas: HashMap<DFARef, Vec<(StateId, Ustr)>> = Default::default();
        for (from, tos) in &self.transitions {
            for (input, _) in tos {
                match input {
                    Input::Nonterminal(_, Some(Specialization { zsh: Some(cmd), .. })) => top_level.push((*from, *cmd)),
                    Input::Subword(subdfa) => {
                        if subdfas.contains_key(subdfa) {
                            continue;
                        }
                        let mut transitions = subdfas.entry(subdfa.clone()).or_default();
                        do_get_subdfa_zsh_command_transitions(subdfa.as_ref(), &mut transitions);
                        continue;
                    },
                    Input::Nonterminal(..) | Input::Command(..) | Input::Literal(..) => {},
                };
            }
        }
        (top_level, subdfas)
    }

    pub fn get_literal_transitions_from(&self, from: StateId) -> Vec<(Ustr, Ustr, StateId)> {
        let map = match self.transitions.get(&StateId::try_from(from).unwrap()) {
            Some(map) => map,
            None => return vec![],
        };
        let transitions: Vec<(Ustr, Ustr, StateId)> = map.iter().filter_map(|(input, to)| match input {
            Input::Literal(input, description) => Some((*input, description.unwrap_or(ustr("")), *to)),
            Input::Subword(..) => None,
            Input::Nonterminal(..) => None,
            Input::Command(..) => None,
        }).collect();
        transitions
    }

    pub fn get_subword_transitions_from(&self, from: StateId) -> Vec<(DFARef, StateId)> {
        let map = match self.transitions.get(&StateId::try_from(from).unwrap()) {
            Some(map) => map,
            None => return vec![],
        };
        let transitions: Vec<(DFARef, StateId)> = map.iter().filter_map(|(input, to)| match input {
            Input::Subword(dfa) => Some((dfa.clone(), *to)),
            Input::Literal(..) => None,
            Input::Nonterminal(..) => None,
            Input::Command(..) => None,
        }).collect();
        transitions
    }


    pub fn get_all_states(&self) -> RoaringBitmap {
        let mut states: RoaringBitmap = Default::default();
        for (from, to) in &self.transitions {
            states.insert((*from).into());
            to.iter().for_each(|(_, to)| {
                states.insert((*to).into());
            });
        }
        states.insert(DEAD_STATE_ID.into());
        states
    }

    pub fn get_match_anything_transitions(&self) -> Vec<(StateId, StateId)> {
        let mut result: Vec<(StateId, StateId)> = Default::default();
        for (from, tos) in &self.transitions {
            for (input, to) in tos {
                if input.matches_anything() {
                    result.push((*from, *to));
                }
            }
        }
        result
    }

    pub fn get_subwords(&self, first_id: usize) -> HashMap<DFARef, usize> {
        let mut unallocated_id = first_id;
        let mut result: HashMap<DFARef, usize> = Default::default();
        for (_, tos) in &self.transitions {
            for (input, _) in tos {
                let dfa = match input {
                    Input::Subword(dfa) => dfa,
                    Input::Nonterminal(..) => continue,
                    Input::Command(..) => continue,
                    Input::Literal(..) => continue,
                };
                result.entry(dfa.clone()).or_insert_with(|| {
                    let save = unallocated_id;
                    unallocated_id += 1;
                    save
                });
            }
        }
        result
    }

    pub fn to_dot<W: Write>(&self, output: &mut W) -> std::result::Result<(), std::io::Error> {
        writeln!(output, "digraph nfa {{")?;
        writeln!(output, "\trankdir=LR;")?;
        do_to_dot(output, self, "", 0)?;
        writeln!(output, "}}")?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn to_dot_file<P: AsRef<std::path::Path>>(
        &self,
        path: P,
    ) -> std::result::Result<(), std::io::Error> {
        let mut file = std::fs::File::create(path)?;
        self.to_dot(&mut file)?;
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::grammar::Expr;
    use crate::grammar::tests::arb_expr_match;
    use crate::regex::AugmentedRegex;
    use Expr::*;

    use super::*;

    use bumpalo::Bump;
    use ustr::{ustr as u, UstrMap};
    use proptest::prelude::*;

    impl Transition {
        fn new(from: StateId, input: &str, to: StateId) -> Self {
            Self {
                from, input: Input::Literal(ustr::ustr(input), None), to
            }
        }
    }

    impl DFA {
        pub fn accepts(&self, inputs: &[&str]) -> Result<bool, TestCaseError>{
            let mut input_index = 0;
            let mut current_state = self.starting_state;
            'outer: loop {
                if input_index == inputs.len() {
                    break;
                }

                for (transition_input, to) in self.iter_transitions_from(current_state) {
                    if let Input::Literal(s, _) = transition_input {
                        if inputs[input_index] == s.as_str() {
                            input_index += 1;
                            current_state = to;
                            continue 'outer;
                        }
                    }
                }

                for (transition_input, to) in self.iter_transitions_from(current_state) {
                    if let Input::Subword(dfa) = transition_input {
                        if dfa.as_ref().accepts_str(inputs[input_index]) {
                            input_index += 1;
                            current_state = to;
                            continue 'outer;
                        }
                    }
                }

                let anys: Vec<(Input, StateId)> = self.iter_transitions_from(current_state).filter(|(input, _)| input.matches_anything()).map(|(k, v)| (k.clone(), v)).collect();
                // It's ambiguous which transition to take if there are two transitions
                // representing a nonterminal.
                prop_assume!(anys.len() <= 1);

                for (transition_input, to) in anys {
                    if transition_input.matches_anything() {
                        input_index += 1;
                        current_state = to;
                        continue 'outer;
                    }
                }

                break;
            }
            Ok(self.accepting_states.contains(current_state.into()))
        }

        fn get_transitions(&self) -> Vec<Transition> {
            let mut result: Vec<Transition> = Default::default();
            for (from, tos) in &self.transitions {
                for (input, to) in tos {
                    result.push(Transition {
                        from: *from,
                        to: *to,
                        input: input.clone(),
                    });
                }
            }
            result
        }

        pub fn has_transition(&self, from: StateId, input: Input, to: StateId) -> bool {
            *self.transitions.get(&from).unwrap().get(&input).unwrap() == to
        }
    }

    #[test]
    fn minimal_example() {
        use ustr::ustr;
        let expr = Terminal(ustr("foo"), None);
        let arena = Bump::new();
        let specs = UstrMap::default();
        let regex = AugmentedRegex::from_expr(&expr, &specs, &arena);
        let dfa = DFA::from_regex(&regex);
        let transitions = dfa.get_transitions();
        assert_eq!(transitions, vec![Transition::new(1, "foo", 2)]);
        assert_eq!(dfa.accepting_states, RoaringBitmap::from_iter([2]));
        assert_eq!(dfa.starting_state, 1);
    }

    const TERMINALS: &[&str] = &["foo", "bar", "--baz", "--quux"];
    const NONTERMINALS: &[&str] = &["FILE", "DIRECTORY", "PATH"];

    proptest! {
        #[test]
        fn accepts_arb_expr_input_from_regex((expr, input) in arb_expr_match(Rc::new(TERMINALS.iter().map(|s| u(s)).collect()), Rc::new(NONTERMINALS.iter().map(|s| u(s)).collect()), 10, 3)) {
            // println!("{:?}", expr);
            // println!("{:?}", input);
            let arena = Bump::new();
            let specs = UstrMap::default();
            let regex = AugmentedRegex::from_expr(&expr, &specs, &arena);
            let dfa = DFA::from_regex(&regex);
            let input: Vec<&str> = input.iter().map(|s| {
                let s: &str = s;
                s
            }).collect();
            prop_assert!(dfa.accepts(&input)?);
        }

        #[test]
        fn minimized_dfa_equivalent_to_input_one((expr, input) in arb_expr_match(Rc::new(TERMINALS.iter().map(|s| u(s)).collect()), Rc::new(NONTERMINALS.iter().map(|s| u(s)).collect()), 10, 3)) {
            println!("{:?}", expr);
            println!("{:?}", input);
            let arena = Bump::new();
            let specs = UstrMap::default();
            let regex = AugmentedRegex::from_expr(&expr, &specs, &arena);
            let dfa = DFA::from_regex(&regex);
            let input: Vec<&str> = input.iter().map(|s| {
                let s: &str = s;
                s
            }).collect();
            prop_assert!(dfa.accepts(&input)?);
            let minimal_dfa = dfa.minimize();
            prop_assert!(minimal_dfa.accepts(&input)?);
        }
    }

    #[test]
    fn engineering_a_compiler_book_dfa_minimization_example() {
        use ustr::ustr;
        let dfa = {
            let starting_state = 0;
            let mut transitions: HashMap<StateId, HashMap<Input, StateId>> = Default::default();
            transitions.entry(0).or_default().insert(Input::Literal(ustr("f"), None), 1);
            transitions.entry(1).or_default().insert(Input::Literal(ustr("e"), None), 2);
            transitions.entry(1).or_default().insert(Input::Literal(ustr("i"), None), 4);
            transitions.entry(2).or_default().insert(Input::Literal(ustr("e"), None), 3);
            transitions.entry(4).or_default().insert(Input::Literal(ustr("e"), None), 5);
            let accepting_states = RoaringBitmap::from_iter([3,5]);
            let input_symbols = Rc::new(HashSet::from_iter([Input::Literal(ustr("f"), None), Input::Literal(ustr("e"), None), Input::Literal(ustr("i"), None)]));
            DFA { starting_state, transitions, accepting_states, input_symbols }
        };
        let minimized = dfa.minimize();
        assert_eq!(minimized.starting_state, 0);
        assert_eq!(minimized.accepting_states, RoaringBitmap::from_iter([3]));
        assert!(minimized.has_transition(0, Input::Literal(ustr("f"), None), 1));
        assert!(minimized.has_transition(1, Input::Literal(ustr("e"), None), 2));
        assert!(minimized.has_transition(1, Input::Literal(ustr("i"), None), 2));
        assert!(minimized.has_transition(2, Input::Literal(ustr("e"), None), 3));
    }

    #[test]
    fn minimization_fails() {
        let (expr, input) = (Alternative(vec![Rc::new(Many1(Rc::new(Alternative(vec![Rc::new(Terminal(u("--quux"), None)), Rc::new(Sequence(vec![Rc::new(Optional(Rc::new(Sequence(vec![Rc::new(Many1(Rc::new(Many1(Rc::new(Alternative(vec![Rc::new(Terminal(u("--baz"), None)), Rc::new(Nonterminal(u("FILE")))])))))), Rc::new(Nonterminal(u("FILE")))])))), Rc::new(Sequence(vec![Rc::new(Nonterminal(u("FILE"))), Rc::new(Terminal(u("foo"), None))]))]))])))), Rc::new(Nonterminal(u("FILE")))]), [u("--quux"), u("--baz"), u("anything"), u("anything"), u("foo")]);
        let arena = Bump::new();
        let specs = UstrMap::default();
        let regex = AugmentedRegex::from_expr(&expr, &specs, &arena);
        let dfa = DFA::from_regex(&regex);
        let input: Vec<&str> = input.iter().map(|s| {
            let s: &str = s;
            s
        }).collect();
        assert!(dfa.accepts(&input).unwrap());
        let minimal_dfa = dfa.minimize();
        assert!(minimal_dfa.accepts(&input).unwrap());
    }

    #[test]
    fn minimization_counterexample1() {
        let (expr, input) = (Alternative(vec![Rc::new(Many1(Rc::new(Sequence(vec![Rc::new(Nonterminal(u("FILE"))), Rc::new(Nonterminal(u("FILE")))])))), Rc::new(Nonterminal(u("FILE")))]), [u("anything"), u("anything"), u("anything"), u("anything"), u("anything"), u("anything")]);
        let arena = Bump::new();
        let specs = UstrMap::default();
        let regex = AugmentedRegex::from_expr(&expr, &specs, &arena);
        let dfa = DFA::from_regex(&regex);
        let input: Vec<&str> = input.iter().map(|s| {
            let s: &str = s;
            s
        }).collect();
        assert!(dfa.accepts(&input).unwrap());
        let minimal_dfa = dfa.minimize();
        assert!(minimal_dfa.accepts(&input).unwrap());
    }

    #[test]
    fn minimization_counterexample2() {
        let (expr, input) = (Sequence(vec![Rc::new(Sequence(vec![Rc::new(Alternative(vec![Rc::new(Many1(Rc::new(Many1(Rc::new(Terminal(u("--baz"), None)))))), Rc::new(Nonterminal(u("FILE")))])), Rc::new(Terminal(u("--baz"), None))])), Rc::new(Many1(Rc::new(Alternative(vec![Rc::new(Nonterminal(u("FILE"))), Rc::new(Nonterminal(u("FILE")))]))))]), [u("anything"), u("--baz"), u("anything"), u("anything")]);
        let arena = Bump::new();
        let specs = UstrMap::default();
        let regex = AugmentedRegex::from_expr(&expr, &specs, &arena);
        let dfa = DFA::from_regex(&regex);
        let input: Vec<&str> = input.iter().map(|s| {
            let s: &str = s;
            s
        }).collect();
        assert!(dfa.accepts(&input).unwrap());
        let minimal_dfa = dfa.minimize();
        assert!(minimal_dfa.accepts(&input).unwrap());
    }
}
