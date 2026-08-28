import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { createAppRouter } from "./router";

/**
 * THE ROUTER'S WAITING SCREEN — the counterpart the router shipped without.
 *
 * `createAppRouter` set a `defaultErrorComponent` and no `defaultPendingComponent`: a page that
 * FAILED had a screen, a page that was LOADING had none, and the slot stayed empty for as long as
 * its data took. These tests hold BOTH halves of the pair, because the second is what made the
 * first necessary — setting a pending component is also what puts the route inside a suspense
 * boundary, so it is a change the ERROR path can feel.
 *
 * NO APP ROUTE DECLARES A LOADER TODAY (PDR-0029 opened the door — `context.queryClient
 * .ensureQueryData` — and nothing has walked through it yet), so the waiting behaviour is exercised
 * on a PROBE tree: a two-route router built with the pending/error components READ OFF the app's own
 * router. That is the load-bearing detail — mutate `createAppRouter`, and the probe loses the very
 * component it renders, so every assertion here answers for the shipped configuration and not for a
 * copy of it.
 */

/**
 * HOW LONG `findBy*` MAY WAIT HERE, and why it is not the default.
 *
 * Testing Library's default is 1000 ms and TanStack's `defaultPendingMs` is also 1000 ms — the very
 * grace this file asserts the app does not override. Waiting for the pending screen with the default
 * is therefore an EQUALITY with no margin: the screen is scheduled at the instant the wait gives up,
 * and any scheduling latency loses the race. Measured on the full unit suite: 1626 / 3 under load,
 * 1629 / 1629 idle, 5 / 5 in isolation — a flake that only shows when the machine is busy, i.e. in
 * the cross-OS gate and nowhere else.
 *
 * Five times the library's own grace: long enough that only a real regression exhausts it, and it
 * costs nothing on the happy path because `findBy*` polls and returns as soon as the node appears.
 */
const PENDING_WAIT = { timeout: 5_000 };

/** A promise the test resolves by hand — a loader stuck exactly as long as the assertions need. */
function gate(): { promise: Promise<void>; open: () => void } {
  let open!: () => void;
  const promise = new Promise<void>((resolve) => {
    open = resolve;
  });
  return { promise, open };
}

/**
 * A minimal router carrying the app's pending + error components, over one route that behaves as
 * `run` says. `createRouter` is called WITHOUT any pending-timing option, exactly like the app's.
 */
function probeRouter(run: () => Promise<unknown>) {
  const { defaultPendingComponent, defaultErrorComponent } = createAppRouter().options;
  const root = createRootRoute({ component: () => <Outlet /> });
  const index = createRoute({
    getParentRoute: () => root,
    path: "/",
    loader: run,
    component: () => <p>the page</p>,
  });
  return createRouter({
    routeTree: root.addChildren([index]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
    defaultPendingComponent,
    defaultErrorComponent,
  });
}

describe("router pending screen", () => {
  it("is configured on the router, beside the error component it was missing next to", () => {
    const { defaultPendingComponent, defaultErrorComponent } = createAppRouter().options;
    expect(
      defaultPendingComponent,
      "`createAppRouter` has no `defaultPendingComponent`: no route gets a suspense boundary and a " +
        "loading page paints nothing at all",
    ).toBeDefined();
    expect(
      defaultErrorComponent,
      "`createAppRouter` lost its `defaultErrorComponent` — a page crash would unmount the shell",
    ).toBeDefined();
  });

  it("paints a route whose loader is still running, instead of an empty slot", async () => {
    const loading = gate();
    render(<RouterProvider router={probeRouter(() => loading.promise)} />);

    // NOT on the first frame: the router keeps an empty slot for `defaultPendingMs` (the library's
    // 1 s grace, so a fast loader never flashes this screen) and only then shows it. What this
    // asserts is that it DOES arrive while the loader is still running — before the change, the
    // slot stayed empty for the whole wait, however long it was.
    expect(
      await screen.findByRole("status", { name: "Loading" }, PENDING_WAIT),
      "a route whose loader has not resolved still renders nothing — the empty slot this component " +
        "exists to replace",
    ).toBeTruthy();
    expect(
      screen.queryByText("the page"),
      "the page rendered although its loader has not resolved — the probe proves nothing",
    ).toBeNull();

    loading.open();
    expect(await screen.findByText("the page", undefined, PENDING_WAIT)).toBeTruthy();
    expect(
      screen.queryByRole("status", { name: "Loading" }),
      "the waiting screen outlived the loader that justified it",
    ).toBeNull();
  });

  it("paints the app background — the same surface the static boot splash left behind", async () => {
    const loading = gate();
    render(<RouterProvider router={probeRouter(() => loading.promise)} />);

    const surface = (await screen.findByRole("status", { name: "Loading" }, PENDING_WAIT)).closest(
      "div",
    );
    expect(surface, "the waiting spinner is not inside a surface of its own").not.toBeNull();
    // `index.html`'s boot splash paints the DARK VALUE of this very token, as a literal — the two
    // are held equal by `boot-splash.guard.test.ts`. Painting anything else here makes the moment
    // React takes over visible as a change of colour.
    expect(
      (surface as HTMLElement).className,
      "the waiting screen does not paint `bg-background`: the hand-over from the static boot splash " +
        "becomes a visible colour change",
    ).toContain("bg-background");
  });

  it("adds no delay of its own — the pending timings are the library's", () => {
    const app = createAppRouter().options;
    // A router that sets NO pending option at all: whatever TanStack fills in for it is the
    // untouched default. Comparing against it (rather than against 1000 / 500 written here) says
    // the right thing — the app contributes no waiting time — and keeps saying it after an upgrade.
    const untouched = createRouter({
      routeTree: createRootRoute({ component: () => <Outlet /> }),
      history: createMemoryHistory({ initialEntries: ["/"] }),
    }).options;

    expect(
      app.defaultPendingMs,
      "the app now overrides `defaultPendingMs` — it holds the current page back before showing " +
        "the waiting screen",
    ).toBe(untouched.defaultPendingMs);
    expect(
      app.defaultPendingMinMs,
      "the app now overrides `defaultPendingMinMs` — it keeps the waiting screen up after the data " +
        "has arrived",
    ).toBe(untouched.defaultPendingMinMs);
  });

  it("leaves the error path exactly where it was: a throwing route still gets its own screen", async () => {
    render(
      <RouterProvider router={probeRouter(() => Promise.reject(new Error("loader blew up")))} />,
    );

    // The SAME copy the router shipped before this change, reached through the same
    // `defaultErrorComponent` — adding a pending component turns a route's boundary into a real
    // `React.Suspense`, and this is what says the error boundary around it still catches.
    expect(
      await screen.findByText("Something went wrong on this screen.", undefined, PENDING_WAIT),
    ).toBeTruthy();
    expect(screen.getByText("loader blew up")).toBeTruthy();
  });
});
