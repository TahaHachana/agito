# Files and dialogs

## Uploading files

`set_input_files` sets the files of an `<input type="file">`, from disk or from memory:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:uploads}}
```

It takes a path (`"report.pdf"`), several (`["a.pdf", "b.pdf"]`), a `FilePayload` with a
name and bytes, a `Vec<FilePayload>`, or `InputFiles::none()` to clear the input. It works on
hidden inputs, which pages often hide behind a styled button.

When a page opens the file picker itself, from a button that calls `input.click()`, wait for
the chooser instead:

```rust,ignore
let chooser = page.wait_for_file_chooser(None); // listening starts here
page.get_by_role("button", Some("Upload")).click().await?;
chooser.await?.set_files("report.pdf").await?;
```

Native file pickers never open during a test.

## Downloads

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:downloads}}
```

A `Download` has the `url()` and `suggested_filename()`, `path()` (which waits for the file to
finish), `save_as(path)` to copy it somewhere, `failure()`, `cancel()` and `delete()`. Each
context downloads into a temporary directory of its own, removed when the context closes,
so save what you want to keep. `page.on_download` sees every download.

## Dialogs

`alert`, `confirm`, `prompt` and `beforeunload` dialogs block a page until they are answered.
Without a handler, agito dismisses them, so a page never hangs. To answer one, register a
handler before the action that opens it:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:dialogs}}
```

A `Dialog` has its `kind`, `message` and `default_prompt`. `accept(Some(text))` answers a
prompt; `dismiss()` cancels.

## Console messages and page errors

```rust,ignore
page.on_console(|message| println!("console.{}: {}", message.kind, message.text));
page.on_page_error(|error| eprintln!("uncaught: {error}"));
```

`on_page_error` sees exceptions the page's scripts did not catch, which often explain a test
that fails for no visible reason.
