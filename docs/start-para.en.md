# PARA: let an AI organize your documents and notes by action

[日本語](start-para.md) · [All introductions](getting-started.en.md)

Have an AI read the documents and notes you have piled up and organize them as notes in the four PARA categories: Projects, Areas, Resources, and Archives. There is one note for each document of use, kept with a copy of the document in the folder of the project or area that uses it. People read the notes in a browser, and agents use the same notes over MCP.

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
| Unit | Pages by topic | A note for each document (key points and use) |
| Top-level sections | Topic sections | Projects, Areas, Resources, Archives, with a folder for each project, area or topic under them |
| Who decides the sections | The AI (added to the categories of `rules.md`) | People list Projects and Areas in `rules.md`; the AI adds topics of Resources |
| Where documents are kept | `raw/` | `資料/` in the folder of the note |
| Archiving | Puts stale pages away | Moves a finished project to Archives with its notes and documents |
| Home | Counts and the pages changed lately | The four categories with their counts, and the list of active Projects |

Commands, screen, and source rules are the same as the Wiki. Build a Wiki for an encyclopedia of knowledge; build PARA to keep your desk in order.

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

`init para` creates a folder for each of the four categories under `site/`, and the rules (`rules.md`).

Then list the projects you are running under `## Projects` of `rules.md`, and the areas you are responsible for under `## Areas`. The AI makes no project or area that is not listed there. Write a deadline as `YYYY-MM-DD`, and `nest lint` tells you when it has passed. Of `rules.md`, the AI adds only the topics under `## Resources`; the rest is yours to write (`nest archive` moves a project's line to `## Archives`).

```markdown
## Projects

- ポンプ更新工事：2026-12-20 までに主ポンプを交換する

## Areas

- 設備の定期保全：故障による停止を年 2 回以下にする
```

`--purpose` says what this PARA is for. You can change it later in `about.purpose` of `KnowledgeNestfile.yaml`.

## 3 Ingest documents

```sh
nest ingress ../inbox
```

The AI reads the documents, writes a note for each document of use, and puts it where it will be used. A note says what the document is, its **key points** for the project or area, and **how to use it**.

1. A document for one of the Projects goes to `Projects/<project>/<document>`
2. A document for the work of one of the Areas goes to `Areas/<area>/<document>`
3. Anything else that may help later goes to `Resources/<topic>/<document>`; a new topic is added under `## Resources` of `rules.md`
4. A document of no use gets no note and is recorded as having nothing to add ("得るものなし")

The AI is given the list of places a note can go to (the projects and areas of the rules, and the topics so far). It decides only the place; the note is named after its document. A document has one note at one place: when the AI writes it at another place as well, the first is kept and the others are warned of (`NOTE_SKIPPED`). A copy of the document is kept in `資料/` beside the note, and the note's source points to it. A note placed in a project that the rules do not list is not saved (`PLACE_INVALID`); its document stays pending, so list the project and run `nest ingress --pending`. Numbers, dates and quotations not found in the document are warned of (`UNSUPPORTED_LITERAL`). Pass a URL, as in `nest ingress https://...`, to ingest a web page. Before that, run `nest browser` once, sign in to the sites to read in the browser of nest it opens, then close it ([details](wiki.md)). If it takes a while, give the job ID to `nest progress <job-id>` to follow it.

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
| Have the wording fixed | `nest instruct <note> "Keep three key points"` |
| Start a new project | Add it under `## Projects` of `rules.md` |
| Add a rule for sorting from now on | `nest instruct --persistent "Put catalogs in Resources"` |
| Put away a finished project | `nest archive Projects/<project>` (moves to `Archives/<project>` with its notes and documents, moves its line of `rules.md` to `## Archives`, and follows the links to its notes; `nest archive --restore Archives/<project>` or `nest undo` brings it back) |
| Put away a single note | `nest archive <note>` |
| Find overdue projects and conflicts | `nest verify`, `nest verify --content` |
| Undo a job | `nest undo <job-id>` |

Archived notes leave ordinary questions. They are not deleted; you can still read them and bring them back.

## 6 When something goes wrong

| Symptom | What to do |
|---|---|
| Notes land in the wrong place | List your projects and areas under `## Projects` and `## Areas` of `rules.md`. Add rules with `instruct --persistent` |
| `PLACE_INVALID` warning | A note for a project or area the rules do not list is not saved. List it and run `nest ingress --pending` |
| `MODEL_REQUIRED` | Set `models.writer.model` |

More in [Wiki](start-wiki.en.md#9-when-something-goes-wrong) and [Troubleshooting](troubleshooting.md) (Japanese).

## Where next

- Instructions, checks, and undo in detail (same as the Wiki): [Wiki](start-wiki.en.md)
- Screen operations: [Wiki and PARA](wiki.md) (Japanese)
