const titleOrder = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });

/** Order the complete browse result, not the rendered window. Unknown dates stay last. */
export function orderEpisodes(episodes, sort = "recent") {
  const ordered = [...episodes];
  if (sort === "title") {
    return ordered.sort((a, b) => titleOrder.compare(a.track.name ?? "", b.track.name ?? ""));
  }
  const direction = sort === "oldest" ? 1 : -1;
  return ordered.sort((a, b) => {
    const aKnown = Number.isFinite(a.published_at);
    const bKnown = Number.isFinite(b.published_at);
    if (aKnown !== bKnown) return aKnown ? -1 : 1;
    return aKnown ? direction * (a.published_at - b.published_at) : 0;
  });
}

/** Normalize once per browse result; sorting keeps these episode identities. */
export function indexEpisodeSearch(episodes) {
  return new Map(episodes.map((episode) => [
    episode,
    [String(episode.track.name ?? "").toLowerCase(), String(episode.description ?? "").toLowerCase()],
  ]));
}

/** Literal, case-insensitive title/description search; keep the chosen order. */
export function filterEpisodes(episodes, query, index) {
  const wanted = query.trim().toLowerCase();
  if (!wanted) return episodes;
  return episodes.filter((episode) => {
    const [name, description] = index.get(episode);
    return name.includes(wanted) || description.includes(wanted);
  });
}
