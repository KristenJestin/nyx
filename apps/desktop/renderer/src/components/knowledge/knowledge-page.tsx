import { BookOpenIcon } from "lucide-react";

import { PagePlaceholder } from "@/components/terminals/page-placeholder";

/**
 * `<KnowledgePage>` — the routed Knowledge surface (`/knowledge`). A navigable STUB for
 * now: the real knowledge base is ported from the prototype in a later pass. It renders a
 * clean, token-only empty state so the section-nav has a real destination today.
 */
export function KnowledgePage() {
  return (
    <PagePlaceholder
      icon={<BookOpenIcon className="size-8" />}
      title="Knowledge"
      description="The knowledge base is coming soon. This page is already routed from the navigation."
    />
  );
}

export default KnowledgePage;
