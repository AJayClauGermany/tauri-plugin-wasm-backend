## Default Permission

Allows the frontend to invoke WASM backend modules and list them.

App modules are invocable unless their config sets `expose: false`; external
plugins need an explicit `allow-invoke` scope entry. Reloading modules and
generating TypeScript bindings are not included.

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
</table>
