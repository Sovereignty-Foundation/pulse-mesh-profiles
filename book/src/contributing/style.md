# Documentation style

<p class="pm-kind"><b>Contributing</b> · for anyone writing or reviewing a page of this book, a README, a crate overview or a message a user will read</p>

The book is held to the same standard as the code: every page has one job, states it, and can be checked. This page is the standard. The review checklist at the end is what a documentation pull request is read against.

## One page, one kind

Every page is exactly one of six kinds and says which in its first line, in the label the theme renders from `<p class="pm-kind"><b>Kind</b> · for whom</p>`.

| Kind | Job | Shape |
|---|---|---|
| Example | Show it working | Opens with what the reader will have and what they need; one self-contained prompt for a coding agent; the same steps by hand with the literal output of each; a table of checks with pass conditions and what failures mean; what happened, including what the code does not do; variations; a spoken script. |
| Workflow | Do one domain's job end to end | An example's shape, for one domain: robotics, enterprise, HFT or distributed compute. States which parts of the path are implemented and which are deferred. |
| Tutorial | Teach by doing | States what it assumes and what the reader will have at the end; numbered steps; every command run by the documentation gate; ends with what was learned. |
| How-to guide | Get a task done | Names the task, the prerequisites, the steps in order with the expected result after each; links out for background and facts; no teaching. |
| Reference | State what is true | Complete, precise, generated from the code where a table can be; no persuasion, no steps. |
| Explanation | Say why | One claim, the mechanism that makes it true, one figure, and a closing section on what would refute it. |

A page that wants to do two jobs is two pages.

## Voice

The voice is the Foundation's, as it stands on the Foundation's website.

- Declarative and precise. State the claim, then the evidence. "A reader that sees an odd counter retries" rather than "readers will generally want to retry".
- One idea per sentence. Short sentences carry weight; a sentence that needs a semicolon is two sentences.
- Institutional, not personal. No "we think", no "I", no "the author". Tutorials and guides address the reader as "you"; reference and explanation pages address no one.
- No superlatives, no hedging, no marketing. "The ring publishes without a system call" rather than "blazingly fast".
- Define a term before using it, and use the glossary's word for it every time. Pulse, PulseEnvelope, the daemon, `pms`, the four tiers, the dev ring: never a synonym.
- Numbers carry units and a source. A measurement names the command that produced it.
- What is not yet true is said plainly. A specified behaviour that the code does not implement yet is named as such, with the test that will prove it.
- Superseded material stays in place and is marked; nothing is silently rewritten. Decisions go to `decisions/` as append-only records.
- Every explanation ends with what would refute it. Refutations are welcome and the page says so.
- American spelling. Present tense. Second person only in tutorials and guides.

## Names that are welcome and names that are not

The Sovereignty stack's names are public and are used deliberately: Canon, the Lexicon, Kronos, Sov OS, Musashi, Knox, Aegis, Sage, RITA, Hermes, Owl, WISP. The glossary maps each to the component that carries it here or says it is not part of this repository.

The names the registry lists as marks (Pulse Mesh™, PulseEnvelope™, Canon™, Lexicon™, Kronos™ and the rest; the list is at sovfound.org/registry) carry ™ on their first prominent use on a page: the hero, a part opener, a glossary entry, the first mention in prose. Later mentions on the same page do not repeat it.

What released text does not carry is the language of how the tree was made rather than what it is: internal phase labels, migration process notes, and names of documents or features that no longer exist. `scripts/docs/lint-terms.py` holds the list and runs in the documentation gate.

## Structure

- Headings are phrased as the reader's question or task in tutorials and guides ("Publish an envelope", "What you will need"), and as the noun they describe in reference and explanation ("Slot layout", "Why two reads").
- Code comes before prose in a how-to guide: the command, then what it does.
- A page stays under about 1,500 words. A page that grows past it splits along its headings.
- Rust samples are included from the examples with `{{#include}}` anchors rather than typed into the page, so the sample and the page cannot drift. Shell transcripts show the command and the output the reader should see.
- Every reference table that can be generated from the code is generated, and the gate regenerates it and fails on a difference.
- One figure per explanation page, from the website's catalogue where one exists, with a caption in the label idiom (`<figure class="pm-figure">`). Colors mean what the legend says: cyan the Pulse, violet the envelope, emerald the substrate, amber the transport, rose an error, slate what is set aside.
- Links are relative and point at pages of this book, at files in the tree, or at the decision record. The gate checks that every link resolves and every page is reachable from the summary.

## Markdown conventions

One paragraph is one line; prose is never hard-wrapped at a column. List items and table rows are one line each. Code blocks name their language. Files begin with a single `#` heading and then the page kind line (part openers and the introduction carry their heading inside the hero block, with the kind line after it).

## The review checklist

A documentation pull request is read against these questions, and each one is answered before it merges.

1. Is the page exactly one kind, and does its first line say which and for whom?
2. Does every command in it run in the documentation gate, and does the output shown match?
3. Is every fact traceable to code, a test, a fixture, or a bench command named on the page?
4. Does an explanation page make one claim, show one figure, and say what would refute it?
5. Are all terms the glossary's terms, used in the glossary's senses?
6. Does the page carry no internal process language, no persona, and no name of something that does not exist?
7. Do the version, the links and the summary pass the gate (`just docs`)?
8. Is anything superseded marked rather than deleted, and is any decision it records in `decisions/`?
9. Is it under the length limit, or split?
10. Would a reader who knows nothing of how this tree was made understand every sentence?
11. Is every claim about what the code does, or lacks, checked against the code itself rather than against a comment, an earlier page or a review? A comment can be wrong from the day it was written, and a claim that is only ever checked against other claims is never refuted.
