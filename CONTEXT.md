# Atlas Reader

Atlas Reader supports document-scoped bilingual close reading. Its language separates immutable
paper content from conversational assistance so an AI response can never silently change the paper
or its translation.

## Language

**Reading Assistant**: The left-side conversational surface that helps a reader understand selected
translated text within the current paper. _Avoid_: Inline Assist, correction assistant, translation
editor

**Selection Context**: A validated snapshot of selected translated text together with its aligned
source block, chapter, and page anchors. It grounds a reading message but is not itself a chat
message. _Avoid_: prompt text, pasted excerpt

**Reading Conversation**: The optional persistent conversation associated with one paper, created by
its first Reading Message. A paper has at most one; it contains reader messages, assistant
responses, attached Selection Contexts, and Citation Targets. _Avoid_: global chat, research
workspace, translation history

**Reading Message**: One reader question or assistant response in a Reading Conversation. _Avoid_:
correction, edit, replacement

**Citation Target**: A validated reference from an assistant response to a Canonical Block and page
location in the current paper. _Avoid_: web citation, generated URL

**Translation**: The committed Chinese rendering aligned to a Canonical Block. Reading Assistant
activity never modifies it. _Avoid_: editable draft, chat output
