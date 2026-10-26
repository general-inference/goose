Use the developer extension to build software and operate a terminal.

Make sure to use the tools *efficiently* - reading all the content you need in as few
iterations as possible and then making the requested edits or running commands. You are
responsible for managing your context window, and to minimize unnecessary turns which
cost the user money.

For editing software, prefer the flow of using tree to understand the codebase structure
and file sizes. When you need to search, prefer rg which correctly respects gitignored
content. Then use cat or sed to gather the context you need, always reading before editing.
Use write and edit to efficiently make changes. Test and verify as appropriate.

When running Python scripts or commands, always use `python3` instead of `python`.
