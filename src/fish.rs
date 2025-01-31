use std::io::Write;

use complgen::{StateId, Result};
use hashbrown::HashMap;
use ustr::{Ustr, UstrMap, ustr};
use crate::dfa::DFA;


// array indices start at 1 in fish , not 0 (!)


pub fn escape_fish_string(s: &str) -> String {
    s.replace("\"", "\\\"").replace("$", "\\$")
}


fn write_tables<W: Write>(buffer: &mut W, dfa: &DFA) -> Result<()> {
    let all_literals: Vec<(usize, Ustr, Ustr)> = dfa.get_all_literals().into_iter().enumerate().map(|(id, (literal, description))| (id + 1, literal, description.unwrap_or(ustr("")))).collect();

    let literal_id_from_input_description: HashMap<(Ustr, Ustr), usize> = all_literals.iter().map(|(id, literal, description)| ((*literal, *description), *id)).collect();
    let literals: String = itertools::join(all_literals.iter().map(|(_, literal, _)| literal), " ");
    writeln!(buffer, r#"    set --local literals {literals}"#)?;
    writeln!(buffer, "")?;

    for (id, _, description) in all_literals.iter() {
        writeln!(buffer, r#"    set descriptions[{id}] "{}""#, escape_fish_string(description))?;
    }
    writeln!(buffer, "")?;

    for state in dfa.get_all_states() {
        let transitions = dfa.get_literal_transitions_from(StateId::try_from(state).unwrap());
        if transitions.is_empty() {
            continue;
        }
        let transitions: Vec<(usize, StateId)> = transitions.into_iter().map(|(input, description, to)| (*literal_id_from_input_description.get(&(input, description)).unwrap(), to)).collect();
        if transitions.is_empty() {
            continue;
        }
        let state_inputs: String = itertools::join(transitions.iter().map(|(literal_id, _)| format!("{}", literal_id)), " ");
        let state_tos: String = itertools::join(transitions.iter().map(|(_, to)| format!("{}", to + 1)), " ");
        // TODO Make two arrays out of transitions: transition_inputs and transition_tos to reduce output size
        writeln!(buffer, r#"    set transitions[{}] "set inputs {state_inputs}; set tos {state_tos}""#, state + 1)?;
    }

    writeln!(buffer, "")?;

    let match_anything_transitions = dfa.get_match_anything_transitions();
    let match_anything_transitions_from = itertools::join(match_anything_transitions.iter().map(|(from, _)| format!("{}", from + 1)), " ");
    writeln!(buffer, r#"    set --local match_anything_transitions_from {match_anything_transitions_from}"#)?;
    let match_anything_transitions_to = itertools::join(match_anything_transitions.iter().map(|(_, to)| format!("{}", to + 1)), " ");
    writeln!(buffer, r#"    set --local match_anything_transitions_to {match_anything_transitions_to}"#)?;

    Ok(())
}


fn write_specialized_commands<W: Write>(buffer: &mut W, command: &str, dfa: &DFA) -> Result<(Vec<(usize, StateId, Ustr)>, HashMap<StateId, usize>)> {
    let specialized_command_transitions: Vec<(usize, StateId, Ustr)> = dfa.get_fish_command_transitions().into_iter().enumerate().map(|(id, (from, input))| (id + 1, from, input)).collect();

    // We can't identify commands by state ids because we're deduplicating them
    let mut specialized_id_from_command: UstrMap<usize> = Default::default();
    let mut specialized_id_from_state: HashMap<StateId, usize> = Default::default();
    for (id, state, command) in &specialized_command_transitions {
        if let Some(canonical_id) = specialized_id_from_command.get(command) {
            specialized_id_from_state.insert(*state, *canonical_id);
        }
        else {
            specialized_id_from_command.insert(*command, *id);
            specialized_id_from_state.insert(*state, *id);
        }
    }

    for (cmd, id) in &specialized_id_from_command {
        write!(buffer, r#"function _{command}_spec_{id}
    set 1 $argv[1]
    {cmd}
end

"#)?;
    }

    Ok((specialized_command_transitions, specialized_id_from_state))
}


fn write_specialized_commands_completion_code<W: Write>(buffer: &mut W, command: &str, specialized_command_transitions: &[(usize, StateId, Ustr)], specialized_id_from_state: &HashMap<StateId, usize>) -> Result<()> {
    if !specialized_command_transitions.is_empty() {
        writeln!(buffer, r#"    set specialized_command_states {}"#, itertools::join(specialized_id_from_state.iter().map(|(state, _)| state + 1), " "))?;
        write!(buffer, r#"    set specialized_command_ids {}"#, itertools::join(specialized_id_from_state.iter().map(|(_, id)| id), " "))?;
        write!(buffer, r#"
    if contains $state $specialized_command_states
        set --local index (contains --index $state $specialized_command_states)
        set --local function_id $specialized_command_ids[$index]
        set --local function_name _{command}_spec_$function_id
        set --local --erase inputs
        set --local --erase tos
        set --local lines (eval $function_name $COMP_WORDS[$COMP_CWORD])
        for line in $lines
            printf '%s\n' $line
        end
    end
"#)?;
    }
    Ok(())
}


pub fn write_completion_script<W: Write>(buffer: &mut W, command: &str, dfa: &DFA) -> Result<()> {
    let command_transitions: Vec<(usize, StateId, Ustr)> = dfa.get_command_transitions().into_iter().enumerate().map(|(id, (from, input))| (id + 1, from, input)).collect();

    // We can't identify commands by state ids because we're deduplicating them
    let mut id_from_command: UstrMap<usize> = Default::default();
    let mut id_from_state: HashMap<StateId, usize> = Default::default();
    for (id, state, command) in &command_transitions {
        if let Some(canonical_id) = id_from_command.get(command) {
            id_from_state.insert(*state, *canonical_id);
        }
        else {
            id_from_command.insert(*command, *id);
            id_from_state.insert(*state, *id);
        }
    }

    for (cmd, id) in &id_from_command {
        write!(buffer, r#"function _{command}_{id}
    set 1 $argv[1]
    {cmd}
end

"#)?;
    }

    let (specialized_command_transitions, specialized_id_from_state) = write_specialized_commands(buffer, command, dfa)?;

    write!(buffer, r#"function _{command}"#)?;

    write!(buffer, r#"
    set COMP_LINE (commandline --cut-at-cursor)
"#)?;

    write!(buffer, r#"
    set COMP_WORDS
    echo $COMP_LINE | read --tokenize --array COMP_WORDS
    if string match --quiet --regex '.*\s$' $COMP_LINE
        set COMP_CWORD (math (count $COMP_WORDS) + 1)
    else
        set COMP_CWORD (count $COMP_WORDS)
    end

"#)?;

    write_tables(buffer, dfa)?;

    write!(buffer, r#"
    set --local state {starting_state}
    set --local word_index 2
    while test $word_index -lt $COMP_CWORD
        if set --query transitions[$state] && test -n $transitions[$state]
            set --local --erase inputs
            set --local --erase tos
            eval $transitions[$state]

            set --local -- word $COMP_WORDS[$word_index]
            if contains -- $word $literals
                set --local word_matched 0
                for literal_id in (seq 1 (count $literals))
                    if test $literals[$literal_id] = $word
                        set --local index (contains --index -- $literal_id $inputs)
                        set state $tos[$index]
                        set word_index (math $word_index + 1)
                        set word_matched 1
                        break
                    end
                end
                if test word_matched -ne 0
                    continue
                end
            end
        end

        if test -n $match_anything_transitions_from[$state]
            set --local index (contains --index -- $state $match_anything_transitions_from)
            set state $match_anything_transitions_to[$index]
            set word_index (math $word_index + 1)
            continue
        end

        return 1
    end
"#, starting_state = dfa.starting_state + 1)?;

    write!(buffer, r#"
    if set --query transitions[$state] && test -n $transitions[$state]
        set --local --erase inputs
        set --local --erase tos
        eval $transitions[$state]
        for literal_id in $inputs
            if test -n $descriptions[$literal_id]
                printf '%s\t%s\n' $literals[$literal_id] $descriptions[$literal_id]
            else
                printf '%s\n' $literals[$literal_id]
            end
        end
    end

"#)?;

    if !command_transitions.is_empty() {
        writeln!(buffer, r#"    set command_states {}"#, itertools::join(id_from_state.iter().map(|(state, _)| state + 1), " "))?;
        write!(buffer, r#"    set command_ids {}"#, itertools::join(id_from_state.iter().map(|(_, id)| id), " "))?;
        write!(buffer, r#"
    if contains $state $command_states
        set --local index (contains --index $state $command_states)
        set --local function_id $command_ids[$index]
        set --local function_name _{command}_$function_id
        set --local --erase inputs
        set --local --erase tos
        set --local lines (eval $function_name $COMP_WORDS[$COMP_CWORD])
        for line in $lines
            printf '%s\n' $line
        end
    end
"#)?;
    }

    write_specialized_commands_completion_code(buffer, command, &specialized_command_transitions, &specialized_id_from_state)?;

    write!(buffer, r#"
    return 0
end

complete --command {command} --no-files --arguments "(_{command})"
"#)?;

    Ok(())
}
