import { BriefcaseIcon } from "lucide-react";

import { PagePlaceholder } from "@/components/terminals/page-placeholder";

/**
 * `<WorkPage>` — the routed Work surface (`/work`). A navigable STUB for now: the real
 * board / specs content is ported from the prototype in a later pass. It renders a clean,
 * token-only empty state so the section-nav has a real destination today.
 */
export function WorkPage() {
  return (
    <PagePlaceholder
      icon={<BriefcaseIcon className="size-8" />}
      title="Work"
      description="The board and specs are coming soon. This page is already routed from the navigation."
    />
  );
}

export default WorkPage;
