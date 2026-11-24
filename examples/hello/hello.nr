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
