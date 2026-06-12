# Build the SPA, then serve the static bundle with nginx. Backend URLs are not
# baked in; docker-entrypoint.sh writes /usr/share/nginx/html/env.js from the
# container env at start, so one image targets any of the three backends.
FROM oven/bun:1 AS build
WORKDIR /app
COPY package.json bun.lock* ./
RUN bun install --frozen-lockfile
COPY . .
# The canonical SDL lives one level up (examples/chatapp/schema.graphql), outside this
# build context, so codegen can't run here — but its output (src/gql) is committed.
# Regenerate only if the schema happens to be present; otherwise build on the checked-in
# types. Either way `bun run build` is what produces the static bundle.
RUN if [ -f ./schema.graphql ] || [ -f ../schema.graphql ]; then bun run codegen; fi \
 && bun run build

FROM nginx:1.27-alpine
COPY --from=build /app/dist /usr/share/nginx/html
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY docker-entrypoint.sh /docker-entrypoint.d/40-env.sh
RUN chmod +x /docker-entrypoint.d/40-env.sh
EXPOSE 80
