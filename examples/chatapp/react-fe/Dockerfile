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
# nginx.conf is a template: the base image's 20-envsubst-on-templates.sh renders
# /etc/nginx/templates/*.template into /etc/nginx/conf.d/ before nginx starts. The
# filter restricts substitution to BACKEND_ORIGIN so nginx's own $uri/$host stay
# literal. BACKEND_ORIGIN is the backend on the internal docker network (set per
# stack in docker-compose); it's where presigned /_forge/blob traffic is proxied.
COPY nginx.conf /etc/nginx/templates/default.conf.template
ENV NGINX_ENVSUBST_FILTER=BACKEND_ORIGIN
COPY docker-entrypoint.sh /docker-entrypoint.d/40-env.sh
RUN chmod +x /docker-entrypoint.d/40-env.sh
EXPOSE 80
