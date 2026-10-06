# AGENTS.md

Guidance for contributors and coding agents working in this repository.

## Writing

These rules apply to all text in the repository: doc comments, inline comments, test names, CLI
help text, log and error messages, the README, commit messages and PR descriptions.

Use direct, literal language. State the action, the fact or the condition. Do not use metaphor,
personification, decorative phrasing or rhetorical contrast.

### Sentences

- Put the subject first and the verb next. Write one fact per sentence.
- Name the real actor: "the function returns", "the loader fails", "the edit is skipped". Do not
  give actions to things that do not act. A table does not "name" a hash, a module does not
  "reach" a bin, and a diagnostic does not "say" something.
- State a condition with "if" or "when" and an explicit subject: "Returns `None` if no archive
  contains the chunk."
- State an outcome with a standard verb: returns, fails, skips, reads, writes, resolves, falls
  back to, defaults to.
- Use the established technical term: index, cache, resolve, fallback, missing, stale, lower,
  validate, dry-run. Do not describe around a term that exists.
- Keep "that" and "which" in relative clauses. Write "the first archive that contains the chunk",
  not "the first archive holding it" or "an entry no chunk declares".
- End a sentence with a specific predicate. Do not end with "is a problem", "says nothing",
  "names nothing" or "is as it was". Say what the code does in that case.
- Do not use literary or idiomatic phrases: "for want of", "came to", "the one that remains",
  "as the game has it".

| Do not write | Write |
| --- | --- |
| The chunks `module` edits, each with its edits. An entry no chunk declares is a problem. | Lowers the `module` representation into a deterministic program. |
| Opens the table of game paths of `store`. A cache without it names nothing. | Opens the `game` table of `store`. If the table is not installed, returns a `WadPaths` that resolves no hash. |
| The game's copy of `chunk`: the one in the first archive that holds it. | Reads `chunk` from the first archive that contains it. |
| Logs what an application came to. | Logs a summary of `outcome`. |
| No edit applied to X. It is as the game has it. | No edit was applied to X. It is unchanged. |
| Whether a property edit was skipped for want of a type. | `true` if any property edit was skipped with the `untypable` reason. |

### Doc comments

- Function: the first sentence starts with a third-person present-tense verb and says what the
  function does: "Returns", "Reads", "Lowers". Each condition, fallback and failure follows as
  its own sentence: "Returns `None` if ...", "Fails if ...".
- Type, field or constant: a noun phrase that names what it is. Add a sentence only for a
  constraint, a unit or an invariant that the name does not show.
- Module: the first sentence says what the module does or provides.
- Do not restate the signature. Do not describe how the code changed over time.

### Inline comments

Write an inline comment to give the reason for the code, or a fact the code depends on. Put the
identifiers it refers to in backticks.

### Test names

Name a test `subject_behavior` or `subject_behavior_condition` in snake case, using the
identifiers from the code: `chunk_reads_from_first_archive_containing_it`,
`read_inside_rejects_path_outside_dir`.

### Messages and help text

- A log or error message says what happened and to which object, then what the user can do: "No
  game directory is set. Pass --game-dir, or run `ritobin-tools config set game_dir <DIR>`".
- Help text for an argument says what the value is, then its default.

### Characters

- Do not use em dashes or en dashes. Use a hyphen with spaces (` - `) or restructure the
  sentence. A numeric range uses a bare hyphen (`1-5`).
- Do not use the section sign. Write "section 6".
