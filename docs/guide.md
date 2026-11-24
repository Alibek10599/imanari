# The Imanari guide

This guide teaches you to write an Imanari program and run it. It starts with the smallest complete app, explains every part of the language, and then walks through what the runtime does for you: ownership, revision checks, safe retries and crash recovery.

The [web version of this guide](https://alibek10599.github.io/imanari-website/guide.html) has a playground under each example. You can edit the program there and use the app it describes without installing anything. [spec.md](spec.md) is the normative reference when you need the exact rule.

## What a program is

An Imanari program is one `.nr` file that declares a whole app. You do not write handlers, SQL or page templates. You declare:

- **Entities**: the records the app stores, and which field names their owner.
- **Actions**: the only operations that touch those records. An action inserts a record, lists records, or updates one.
- **Screens**: pages made of forms, lists and buttons, each bound to an action.
- **Policy**: who signs in, the request limits, how often a crashed backend may restart, and what an automated repair may change.

The compiler checks the file and turns it into a native Rust backend, a browser UI, a typed description of the app (`ir.json`) and a dependency graph.

Imanari has no functions, loops or expressions in 0.1. Every program is a set of declarations, and the three action kinds are the whole executable vocabulary. That is deliberate: the compiler can prove what each action reads and writes, so it can generate the access checks instead of trusting you to remember them.

## Install

Imanari compiles apps with Cargo, so install Rust with [rustup](https://rustup.rs) first. Then install the `nari` command. On macOS with Apple silicon:

```sh
curl -fsSL https://github.com/Alibek10599/imanari/releases/latest/download/imanari-aarch64-apple-darwin.tar.gz | tar -xz
sudo install imanari-aarch64-apple-darwin/nari /usr/local/bin/nari
```

On Linux, use the `imanari-x86_64-unknown-linux-gnu` or `imanari-aarch64-unknown-linux-gnu` archive the same way. To build from source instead:

```sh
cargo install --locked --git https://github.com/Alibek10599/imanari imanari
```

Check that it works:

```sh
nari --version
```

## Hello, world

Imanari has no `print`. The smallest useful program is an app that stores a line of text for the signed-in user and shows it back. This is [examples/hello/hello.nr](../examples/hello/hello.nr):

```nari
imanari "0.1";

app hello {
  targets = [web];

  identity development {
    mode = local_dev;
  }

  entity Greeting {
    owner_field = owner;

    field text {
      type = Text;
      min_length = 1;
      max_length = 80;
    }

    field owner {
      type = UUID;
    }
  }

  action say {
    kind = insert;
    entity = Greeting;
    access = signed_in;

    input text {
      type = Text;
      min_length = 1;
      max_length = 80;
    }

    set = {
      text: @input.text,
      owner: @actor.id,
    };
  }

  action greetings {
    kind = list;
    entity = Greeting;
    access = signed_in;
  }

  screen home {
    route = "/";
    title = "Hello, world";
    query = greetings;

    form say_hello {
      submit = say;

      input text {
        label = "Say something";
        placeholder = "Hello, world";
      }
    }

    list said {
      source = @query.items;
      fields = [text];
    }
  }

  runtime local {
    bind = "127.0.0.1";
    port = 7332;
    database = ".nari/app.db";
    request_limit_bytes = 65536;
    request_timeout_ms = 5000;
  }

  recovery backend_process {
    target = backend;
    max_restarts = 3;
    window_seconds = 60;
    backoff_ms = [100, 500, 2000];
    after_exhaustion = stop;
  }

  repair local_candidate {
    mode = isolated;
    max_attempts = 0;
    editable_actions = [];
    automatic_release = false;
  }
}
```

Read it top to bottom:

1. `imanari "0.1";` names the language version. Every file starts with it.
2. `app hello { … }` holds everything else. `targets = [web];` is the only target in 0.1.
3. `identity` says how people sign in. `local_dev` gives you two seeded users on your own machine.
4. `entity Greeting` is the stored record. It has a `text` and an `owner`.
5. `action say` inserts a greeting. The text comes from the request and the owner comes from the session.
6. `action greetings` lists the signed-in user's greetings.
7. `screen home` is the page: a form that calls `say`, and a list fed by `greetings`.
8. `runtime`, `recovery` and `repair` are required policy blocks. They are explained under [Policy blocks](#policy-blocks).

## Run it

Put the program in a directory of its own. A project is a directory with exactly one `.nr` file.

```sh
mkdir hello && cd hello
curl -fsSLO https://raw.githubusercontent.com/Alibek10599/imanari/main/examples/hello/hello.nr
nari check
nari dev
```

`nari check` validates the file offline and prints `ok: hello.nr is valid`. `nari dev` builds the app, starts it under a supervisor, and prints a line like this:

```
open: http://127.0.0.1:7332/#bootstrap=…
```

Open that URL. The page signs you in as User A, removes the one-time token from the address bar, and shows the form. Type a greeting and press Save. The bar at the top switches between User A and User B. Stop the app with Ctrl-C.

The first build compiles the backend's dependencies in release mode and takes about a minute. Later builds are fast. To start from a larger example instead, run `nari init myapp`, which creates `myapp/` with a task-list app and its tests.

These are the commands you will use while writing a program:

| Command | What it does |
| --- | --- |
| `nari check` | Parses and validates the file without building anything |
| `nari fmt` | Rewrites the file in the standard layout and keeps your comments; `--check` only reports |
| `nari build` | Writes `.nari/ir.json`, `.nari/graph.json`, the generated Rust and web files, and compiles them |
| `nari dev` | Builds, then runs the app under the supervisor and prints the sign-in URL |
| `nari test` | Runs the generated contract checks and your protected tests against a real build |
| `nari status` | Shows whether the app is running, its restart history and per-action request counts |

Every command accepts `--project PATH` to work on another directory and `--json` for machine-readable output.

## Syntax

The whole grammar is small enough to remember.

- A **property** is `name = value;`.
- A **block** is `kind name { … }` and contains properties and other blocks.
- A **comment** starts with `//` and runs to the end of the line.

Values are one of these:

| Value | Example | Notes |
| --- | --- | --- |
| String | `"My tasks"` | JSON escaping; no interpolation |
| Integer | `200` | Whole numbers only |
| Boolean | `true`, `false` | |
| Symbol | `Text`, `insert`, `Task` | A bare name: a type, a keyword value, or something you declared |
| Reference | `@input.title` | Points at a value the runtime supplies |
| Array | `[title, completed]` | A trailing comma is allowed |
| Object | `{ completed: true }` | A trailing comma is allowed |

Names use ASCII letters, digits and underscores, and cannot start with a digit. A name must be unique among its siblings of the same kind. Unknown properties, unknown blocks and duplicate keys are errors, so a typo never silently does nothing.

There are four references, and each is valid in one place only:

| Reference | Valid in | Meaning |
| --- | --- | --- |
| `@input.NAME` | An action's `set`, `id` and `expected_revision` | A declared input of that action |
| `@actor.id` | An insert action's `set`, for the owner field | The signed-in user's id, taken from the session |
| `@query.items` | A list's `source` | The records returned by the screen's query |
| `@item.FIELD` | A button's `arguments` | A field of the record the button sits next to |

## Entities and fields

An entity is a kind of record. Each `field` block declares one stored value.

```nari
entity Task {
  owner_field = owner;

  field title {
    type = Text;
    min_length = 1;
    max_length = 200;
  }

  field owner {
    type = UUID;
  }

  field completed {
    type = Bool;
    default = false;
  }
}
```

There are four types:

| Type | Holds | Options |
| --- | --- | --- |
| `Text` | A string | `min_length` and `max_length`, counted in characters; the defaults are 0 and 4096 |
| `Int` | A whole number | Limited to ±9,007,199,254,740,991 so every value is exact in a browser |
| `Bool` | `true` or `false` | |
| `UUID` | A lowercase UUID | |

There are no nullable values, floats or implicit casts. A field may have a `default`, which must fit the field's type and bounds.

Every entity also has two fields you do not declare and cannot assign: `id`, a UUID the runtime generates, and `revision`, an integer that starts at 1 and goes up by one on every update.

`owner_field` names the UUID field that says whose record it is. It is required. Every generated query filters on it, which is how [ownership](#ownership) works.

## Actions

An action is a named operation on one entity. Every action has `kind`, `entity` and `access = signed_in`. Mutating actions declare their inputs with `input` blocks, which take the same `type` and bounds as fields. All inputs are required, and a request with an unknown input field is rejected.

| Kind | You write | What it does |
| --- | --- | --- |
| `insert` | `set` | Creates one record owned by the caller and returns it |
| `list` | nothing more | Returns the caller's records, 50 per page by default |
| `update` | `id`, `expected_revision`, `set` | Changes one of the caller's records if its revision still matches, and returns it |

An insert assigns every field that has no default. The owner field must be assigned `@actor.id`; anything else is a compile error.

```nari
action create_task {
  kind = insert;
  entity = Task;
  access = signed_in;

  input title {
    type = Text;
    min_length = 1;
    max_length = 200;
  }

  set = {
    title: @input.title,
    owner: @actor.id,
  };
}
```

An update names the record with `id` and the revision the caller last saw with `expected_revision`. Its `set` may use inputs or literal values. It cannot assign the owner, the id or the revision.

```nari
action complete_task {
  kind = update;
  entity = Task;
  access = signed_in;

  input id {
    type = UUID;
  }

  input revision {
    type = Int;
  }

  id = @input.id;
  expected_revision = @input.revision;
  set = { completed: true };
}
```

A list action takes no inputs of its own. The transport adds `limit` (1 to 100) and `after` (a cursor) for pagination.

Each action becomes one HTTP endpoint, `POST /api/actions/NAME`, with a JSON object as the body. Inserts and updates also require an `Idempotency-Key` header, which is what makes [retries safe](#safe-retries). The generated browser client sends all of this for you.

## Screens

A screen is one page. It has a `route`, a `title`, and a `query` that names a list action.

```nari
screen tasks {
  route = "/";
  title = "My tasks";
  query = list_tasks;

  form add_task {
    submit = create_task;

    input title {
      label = "Task title";
      placeholder = "What needs doing?";
    }
  }

  list task_list {
    source = @query.items;
    fields = [title, completed];

    button complete {
      label = "Mark complete";
      action = complete_task;
      arguments = {
        id: @item.id,
        revision: @item.revision,
      };
    }
  }
}
```

- A **form** submits to an insert action. Its `input` blocks match the action's inputs by name and add a `label` and an optional `placeholder`. The control type, the required mark and the length limits come from the action.
- A **list** shows the query's records. `fields` picks the columns.
- A **button** inside a list calls an update action for that row. `arguments` must bind every input of the action, either to `@item.FIELD` or to a literal.

The generated page uses native browser controls. It shows loading, empty and error states, blocks double submission, keeps your input when a request fails, and reloads the list after a change. Record content is always rendered as text, never as HTML.

## Policy blocks

Every app has exactly one of each of these four blocks.

`identity` chooses how users sign in. `mode = local_dev` is for development on your own machine. `mode = oidc` with an `issuer` and a `client_id` signs users in through an OpenID Connect provider.

`runtime` holds the limits the backend enforces on every request:

| Property | Allowed | Meaning |
| --- | --- | --- |
| `bind` | `"127.0.0.1"` | The listen address; deployments override it from outside |
| `port` | 1024 to 65535 | The listen port |
| `database` | A path under `.nari/` | The SQLite file |
| `request_limit_bytes` | 1 to 65536 | Largest accepted request body |
| `request_timeout_ms` | 100 to 30000 | How long one request may run |

`recovery` is the restart budget for a crashed backend. `backoff_ms` needs one entry per allowed restart. [Crash recovery](#crash-recovery) shows it working.

`repair` limits what an automated fix may touch. `editable_actions` lists the actions whose `set` bindings a repair may change. `max_attempts = 0` with an empty list, as in the hello program, switches repair off. `automatic_release` must be `false`: a validated fix is only applied when you run `nari apply-repair`.

## Ownership

Every record has an owner, and every action runs as the signed-in user. The compiler adds the owner to each query it generates:

- An insert writes the caller's id into the owner field. The browser never sends it.
- A list returns only rows whose owner is the caller.
- An update only matches a row whose owner is the caller. Someone else's record looks exactly like a record that does not exist, and the answer is `404 NOT_FOUND`.

You cannot write an action that skips the check, because there is no syntax for it. To see it, run the task app and add a task as User A, then switch to User B: the list is empty.

## Revisions and conflicts

Two people, or two browser tabs, can load the same record and both try to change it. An update carries the revision its caller last saw. The backend changes the row only if that revision is still current, and then increments it.

If the revision moved on, the request fails with `409 REVISION_CONFLICT` and nothing is written. The generated page tells the user the item changed and reloads the list. A later write never silently overwrites an earlier one.

## Safe retries

A request can commit on the server and still fail to reach the browser: the connection drops, or the backend crashes after the commit. The client cannot tell whether the change happened.

Every insert and update carries an `Idempotency-Key`. The backend stores the key and the response in the same transaction as the change. When the same user repeats the same action with the same key and the same input, the backend returns the stored response and writes nothing. The generated client keeps a request's key until it gets a definite answer, so pressing Save again after a network error cannot create a duplicate.

Reusing a key with different input is a mistake, and gets `409 IDEMPOTENCY_CONFLICT`.

## Crash recovery

`nari dev` runs the backend as a child of a separate supervisor process. When the backend crashes, the supervisor waits for the next delay in `backoff_ms` and starts it again. Committed data is in the database, so it is still there after the restart.

The budget is `max_restarts` within `window_seconds`. Once it is spent, the supervisor stops, records an incident and reports `StoppedNeedsAttention` instead of restarting forever. `nari status` shows the state, and `nari dev --acknowledge` clears it after you have looked at the cause.

You can inject a crash to watch this happen:

```sh
NARI_FAULT=crash_on_request:create_task nari dev
```

## Testing

`nari test` builds the app and runs two sets of checks against it over real HTTP.

The first set is generated from the program. For every action it checks that an unauthenticated request, a missing CSRF token, an unknown field and an out-of-bounds input are all rejected, and that one user cannot see another's records.

The second set is yours. Each file in `acceptance/` is a scenario written as data:

```json
{
  "name": "a greeting is saved for its author only",
  "covers": ["hello/action/say", "hello/action/greetings"],
  "steps": [
    { "as": "a", "action": "say", "input": { "text": "Hello, world" }, "expect": { "status": 200, "data": { "text": "Hello, world" } } },
    { "as": "b", "action": "say", "input": { "text": "" }, "expect": { "status": 400 } }
  ]
}
```

Each step calls an action as user `a` or `b`. `expect.data` is a subset match. `"save": "task"` keeps a response so that a later step can use `"$task.id"`. These files are called protected tests because an automated repair is never allowed to change them.

## Reading errors

Diagnostics name the file, line and column, an error code, and the id of the declaration at fault:

```
hello.nr:37:14: error E_TYPE: input 'text' is Text but field 'owner' is UUID (no implicit casts) [hello/action/say]
hello.nr:63:17: error E_UNRESOLVED: entity Greeting has no field 'txt' [hello/screen/home/list/said]
```

The first came from assigning `owner: @input.text`. The second came from a list that asked for a field the entity does not have. `nari check --json` gives the same information with byte spans for editors and agents.

The id in brackets is stable. `hello/action/say` stays the same when lines move, and the same id names that action in `ir.json`, the dependency graph, test coverage and incident records.

## Changing a program

Edit the file and run `nari dev` again. When the app starts against a database from an older version of the program, additive changes are applied automatically: new entities, new fields that have a default, and wider text bounds.

Changes that would lose or reinterpret stored data stop startup with `MIGRATION_REQUIRED`. That covers removing or renaming a field, changing a type, and adding a field without a default. `nari diff --base PATH` compares two versions of a program and says which changes apply on their own and which block.

## What 0.1 leaves out

These are rejected with a diagnostic rather than half-supported:

- Public or anonymous access, and sharing a record between users.
- Delete actions, list filters and sorting.
- Floats, dates, nullable fields and references between entities.
- Custom Rust or JavaScript, workflows and calls to external services.
- Mobile targets.

For deployment with Docker, sign-in with OpenID Connect and the repair pipeline, continue with the [README](../README.md). For exact rules, see the [specification](spec.md).
