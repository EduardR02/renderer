import { expect, test } from "bun:test";
import { parseSpotifyLink, spotifyLink } from "./spotify-link.js";

/* An id of the shape Spotify issues, so the builder is exercised against text
   the parser's own rules accept. */
const ID = "4cOdK2wGLETKBW3PvgPWqT";

test("every supported kind builds its canonical open.spotify.com page", () => {
  expect(spotifyLink("track", ID)).toBe(`https://open.spotify.com/track/${ID}`);
  expect(spotifyLink("album", ID)).toBe(`https://open.spotify.com/album/${ID}`);
  expect(spotifyLink("artist", ID)).toBe(`https://open.spotify.com/artist/${ID}`);
  expect(spotifyLink("playlist", ID)).toBe(`https://open.spotify.com/playlist/${ID}`);
});

test("a resource carrying only its URI still yields its id", () => {
  expect(spotifyLink("track", `spotify:track:${ID}`)).toBe(`https://open.spotify.com/track/${ID}`);
  expect(spotifyLink("playlist", `spotify:playlist:${ID}`)).toBe(`https://open.spotify.com/playlist/${ID}`);
});

test("what this app copies is what this app parses back", () => {
  for (const kind of ["track", "album", "artist", "playlist"]) {
    expect(parseSpotifyLink(spotifyLink(kind, ID))).toEqual({ kind, id: ID });
  }
});

test("nothing to build from removes the link instead of guessing one", () => {
  expect(spotifyLink("episode", ID)).toBe("");
  expect(spotifyLink("track", "")).toBe("");
  expect(spotifyLink("track", "spotify:track:")).toBe("");
  expect(spotifyLink("track", null)).toBe("");
  expect(spotifyLink("track", undefined)).toBe("");
});
