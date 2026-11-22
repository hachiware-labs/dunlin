# PARA: let an AI organize your documents and notes by action

[日本語](start-para.md) · [All introductions](getting-started.en.md)

Have an AI read the documents and notes you have piled up and organize them as notes in the four PARA categories: Projects, Areas, Resources, and Archives. Every sentence in a note carries a citation to the original. People read the notes in a browser, and agents use the same notes over MCP.

## The PARA idea

PARA is a way to organize digital information proposed by Tiago Forte ([an introduction](https://fortelabs.com/the-p-a-r-a-method-a-universal-system-for-organizing-digital-information-75a9da8bfb37/)).

Its core idea is to sort **not by topic, but by how close the information is to action**. Sort by topic — "pumps", "rules", "customer A" — and folders keep multiplying while what you need now gets buried. PARA sorts by "will I use this in my work now?" into four categories.

| Category | What goes in | Does it end? | Example |
|---|---|---|---|
| **Projects** | Efforts with a goal and a deadline | Yes | Next month's pump replacement, preparing training for newcomers |
| **Areas** | Ongoing responsibilities | No | Routine equipment maintenance, safety management |
| **Resources** | Reference that may help someday | No | A parts maker's catalog, industry standards |
| **Archives** | Things from the other three that are no longer active | — | Records of finished work, an area you no longer own |

The order runs from closest to action at the top to farthest at the bottom.

- **Projects vs. Areas** is about whether it ends. "Finish the replacement" is a Project; "keep the equipment running" is an Area.
- **Areas vs. Resources** is about responsibility. A field where you keep a standard is an Area; a topic you are merely interested in is a Resource.
- **Information moves.** When the replacement is done, its material moves from Projects to Archives. The categories are not fixed shelves; they reflect your work right now.

## How it differs from the Wiki

| | Wiki | PARA |
|---|---|---|
| Unit | Pages by topic | Notes |
| Top-level sections | Topic sections | Projects, Areas, Resources, Archives |
| Archiving | Puts stale pages away | Moves finished efforts to Archives |
| Home | Counts and the pages changed lately | The four categories with their counts, and the list of active Projects |

Commands, screen, and citation rules are the same as the Wiki. Build a Wiki for an encyclopedia of knowledge; build PARA to keep your desk in order.

## When to use it

- Research notes and documents are piling up and you can't find what's where
- You want each project's documents apart from those of work you own long-term
- You want to put away finished projects' documents without deleting them

## 1 Prepare a writing model

This page uses `qwen3:4b` on a local Ollama.

```sh
nest config set llm --provider ollama --model qwen3:4b
```

## 2 Create PARA

```sh
nest init para ./para --purpose "Organize documents and notes for my equipment maintenance work"
cd para
```

`init para` creates a folder for each of the four categories under `site/`.

`--purpose` is what the AI goes by when it chooses a category. Write the efforts you are running and the areas you own, and the AI tells Projects from Areas more easily — for example, "Running next month's pump replacement; responsible for routine equipment maintenance". You can change it later in `about.purpose` of `KnowledgeNestfile.yaml`.

## 3 Ingest documents

```sh
nest ingress ../inbox
```

The AI reads the documents, writes notes, and files each under one of the four categories. Pass a URL, as in `nest ingress https://...`, to ingest a web page. The citations of what it wrote are checked, and sentences that do not match the source word for word are not saved. If it takes a while, give the job ID to `nest progress <job-id>` to follow it.

## 4 Read: in a browser and over MCP

```sh
nest serve
```

`serve` offers the notes both on the web and over MCP.

- **In a browser**: open `http://127.0.0.1:4319`. Projects, Areas, Resources, and Archives sit at the top of the file tree on the left. The note or original you open shows on the right, with what you can do with it (ask, revise by instruction, check). Give a URL or a file to **資料を取り込む** (add documents) at the top left to ingest it. The home shows the purpose, a box for questions, the four categories with their note counts, and the notes changed lately.
- **From an agent**: register it in your MCP client's settings, and Claude or another agent answers from the notes you have organized.

## 5 Keep using it

| You want to | Command |
|---|---|
| Ask a question answered from the notes | `nest query "Which parts are replaced in next month's work?"` |
| Have a category or wording fixed | `nest instruct <note> "Move this to Areas"` |
| Add a rule for sorting from now on | `nest instruct --persistent "Put catalogs in Resources"` |
| Put away a finished effort | `nest archive <note>` (moves to Archives; `--restore` brings it back) |
| Find broken citations and conflicts | `nest verify`, `nest verify --content` |
| Undo a job | `nest undo <job-id>` |

Archived notes leave ordinary questions. They are not deleted; you can still read them and bring them back.

## 6 When something goes wrong

| Symptom | What to do |
|---|---|
| Notes land in the wrong category | Write your active efforts and areas in `--purpose`. Add rules with `instruct --persistent` |
| `MODEL_REQUIRED` | Set `models.writer.model` |
| `INVALID_CITATION` | None of the citations matched the documents. Run `ingress` again, or use a larger model |

More in [Wiki](start-wiki.en.md#9-when-something-goes-wrong) and [Troubleshooting](troubleshooting.md) (Japanese).

## Where next

- Instructions, checks, and undo in detail (same as the Wiki): [Wiki](start-wiki.en.md)
- Screen operations: [Wiki and PARA](wiki.md) (Japanese)
