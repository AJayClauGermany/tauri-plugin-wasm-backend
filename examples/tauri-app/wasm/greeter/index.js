// A backend module written in plain JavaScript, compiled to a WebAssembly
// component. Edit this file while `pnpm tauri dev` runs: it is rebuilt and
// hot reloaded without recompiling the Rust app.
import { log, emit } from 'tauri:backend/host@0.1.0';
// Host commands imported in wit/world.wit: typed, no JSON strings.
import { appVersion } from 'example:greeter/app';

let count = 0;

export function greet(name) {
  log('info', `greeting ${name}`);
  // Allowed by `capabilities.events.emit` in tauri.conf.json.
  emit('greeter://greeted', JSON.stringify({ name }));
  return `Hello, ${name}! You've been greeted from JavaScript running in WASM!`;
}

// Returning `result<string, string>`: a thrown string rejects the
// frontend's promise, like a Rust command returning `Err`.
export function describe(person) {
  if (!person.name) throw 'a person needs a name';
  // A host command registered in Rust with `Builder::host_command`.
  const version = appVersion();
  return `${person.name} is ${person.ageYears} years old (app v${version})`;
}

export function increment(by) {
  count += by;
  return count;
}

export const events = {
  // Receives the events listed in `capabilities.events.listen`.
  onEvent(event, payload) {
    log('info', `received ${event}: ${payload}`);
    emit('greeter://pong', JSON.stringify({ count, received: JSON.parse(payload) }));
  },
};
