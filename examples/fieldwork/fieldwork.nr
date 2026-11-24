imanari "0.1";

app fieldwork {
  targets = [web];

  identity development {
    mode = local_dev;
  }

  entity Task {
    owner_field = owner;

    // id and revision are supplied by the runtime.
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

  action list_tasks {
    kind = list;
    entity = Task;
    access = signed_in;
  }

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

  runtime local {
    bind = "127.0.0.1";
    port = 7331;
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
    max_attempts = 2;
    editable_actions = [complete_task];
    automatic_release = false;
  }
}
