## Default Permission

Allows the frontend to invoke WASM backend modules and list them.

App modules are invocable unless their config sets `expose: false`; external
plugins need an explicit `allow-invoke` scope entry. Managing modules (see
`manage-modules`) and generating TypeScript bindings are not included.

#### This default permission set includes the following:

- `allow-invoke`
- `allow-list-modules`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`wasm-backend:allow-disable-module`

</td>
<td>

Enables the disable_module command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:deny-disable-module`

</td>
<td>

Denies the disable_module command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:allow-enable-module`

</td>
<td>

Enables the enable_module command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:deny-enable-module`

</td>
<td>

Denies the enable_module command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:allow-invoke`

</td>
<td>

Enables the invoke command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:deny-invoke`

</td>
<td>

Denies the invoke command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:allow-list-inactive-modules`

</td>
<td>

Enables the list_inactive_modules command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:deny-list-inactive-modules`

</td>
<td>

Denies the list_inactive_modules command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:allow-list-modules`

</td>
<td>

Enables the list_modules command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:deny-list-modules`

</td>
<td>

Denies the list_modules command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:allow-reload-module`

</td>
<td>

Enables the reload_module command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:deny-reload-module`

</td>
<td>

Denies the reload_module command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:allow-typescript`

</td>
<td>

Enables the typescript command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:deny-typescript`

</td>
<td>

Denies the typescript command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`wasm-backend:manage-modules`

</td>
<td>

Allows a window to manage modules, e.g. a plugin manager in the app's
settings: list the modules that are and are not running, switch modules off
and on, and reload them.

Not part of the default permission set. The load gate and the external
plugins' allow and deny lists still decide what may load. To limit which
modules a window may manage, allow the single commands with a scope instead.


</td>
</tr>
</table>
