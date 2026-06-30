import { describe, expect, it, vi } from "vitest";
import {
  createHttpTransport,
  createMemoryTransport,
  createSeafClient,
  frameworkName,
} from "../src/index";

describe("@seaf/sdk", () => {
  it("exports the framework name", () => {
    expect(frameworkName).toBe("Self-Evolving Application Framework");
  });

  it("emits typed events through the configured transport", async () => {
    const transport = createMemoryTransport();
    const client = createSeafClient({
      source: "adaptive-notes",
      transport,
      clock: () => new Date("2026-06-30T00:00:00.000Z"),
      idGenerator: () => "evt_1",
    });

    const event = await client.event("note.created", {
      source: "empty_state_button",
    });

    expect(event).toEqual({
      event_id: "evt_1",
      name: "note.created",
      timestamp: "2026-06-30T00:00:00.000Z",
      source: "adaptive-notes",
      privacy_level: "aggregated",
      payload: { source: "empty_state_button" },
    });
    expect(transport.events).toEqual([event]);
  });

  it("wraps metrics and feedback with privacy defaults", async () => {
    const transport = createMemoryTransport();
    const client = createSeafClient({
      source: "adaptive-notes",
      transport,
      clock: () => new Date("2026-06-30T00:00:00.000Z"),
      idGenerator: vi
        .fn()
        .mockReturnValueOnce("evt_metric")
        .mockReturnValueOnce("evt_feedback"),
    });

    await client.metric("startup.p95_ms", 842);
    await client.feedback({
      surface: "empty_state",
      sentiment: "confused",
      message: "I did not realize I could start typing.",
    });

    expect(transport.events[0]).toMatchObject({
      event_id: "evt_metric",
      name: "startup.p95_ms",
      privacy_level: "aggregated",
      payload: { value: 842 },
    });
    expect(transport.events[1]).toMatchObject({
      event_id: "evt_feedback",
      name: "feedback.submitted",
      privacy_level: "private",
      payload: {
        surface: "empty_state",
        sentiment: "confused",
      },
    });
  });

  it("does not allow feedback to downgrade raw message privacy", async () => {
    const transport = createMemoryTransport();
    const client = createSeafClient({
      source: "adaptive-notes",
      transport,
      idGenerator: () => "evt_feedback",
    });

    await client.feedback(
      {
        surface: "empty_state",
        sentiment: "confused",
        message: "I did not realize I could start typing.",
      },
      { privacyLevel: "public" },
    );

    expect(transport.events[0]).toMatchObject({
      name: "feedback.submitted",
      privacy_level: "private",
    });
  });

  it("allows feedback to remain sensitive", async () => {
    const transport = createMemoryTransport();
    const client = createSeafClient({
      source: "adaptive-notes",
      transport,
      idGenerator: () => "evt_feedback",
    });

    await client.feedback(
      {
        surface: "empty_state",
        sentiment: "confused",
        message: "I did not realize I could start typing.",
      },
      { privacyLevel: "sensitive" },
    );

    expect(transport.events[0]).toMatchObject({
      name: "feedback.submitted",
      privacy_level: "sensitive",
    });
  });

  it("posts events to the local runtime endpoint", async () => {
    const fetchMock = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(null, { status: 202 }));
    const transport = createHttpTransport("http://127.0.0.1:7373/");
    const event = {
      event_id: "evt_1",
      name: "note.created",
      timestamp: "2026-06-30T00:00:00.000Z",
      source: "adaptive-notes",
      privacy_level: "aggregated" as const,
      payload: { source: "empty_state_button" },
    };

    await transport.send(event);

    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1:7373/v1/events",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify(event),
      }),
    );
    fetchMock.mockRestore();
  });

  it("rejects malformed event envelopes before transport", async () => {
    const transport = createMemoryTransport();
    const client = createSeafClient({
      source: "adaptive-notes",
      transport,
      idGenerator: () => " ",
    });

    await expect(client.event("note.created")).rejects.toThrow(
      "event_id must not be empty",
    );
    expect(transport.events).toEqual([]);
  });

  it("rejects invalid privacy levels at runtime", async () => {
    const transport = createMemoryTransport();
    const client = createSeafClient({
      source: "adaptive-notes",
      transport,
      idGenerator: () => "evt_1",
    });

    await expect(
      client.event("note.created", {}, { privacyLevel: "raw" as never }),
    ).rejects.toThrow("privacy_level is invalid");
    expect(transport.events).toEqual([]);
  });
});
