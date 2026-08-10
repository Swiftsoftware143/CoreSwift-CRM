"use client";

import { useState } from "react";
import {
  DndContext,
  DragEndEvent,
  DragOverlay,
  DragStartEvent,
  useDraggable,
  useDroppable,
} from "@dnd-kit/core";
import { CSS } from "@dnd-kit/utilities";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { QuickLogActivity, QuickSetFollowUp, QuickMoveStage } from "./quick-actions";

interface Deal {
  id: string;
  company: string;
  value: number;
  stage: string;
  contact?: string;
  followUp?: string;
}

interface Column {
  id: string;
  title: string;
  deals: Deal[];
}

const INITIAL_COLUMNS: Column[] = [
  { id: "prospecting", title: "Prospecting", deals: [] },
  { id: "qualified", title: "Qualified", deals: [] },
  { id: "proposal", title: "Proposal", deals: [] },
  { id: "negotiation", title: "Negotiation", deals: [] },
  { id: "closed", title: "Closed Won", deals: [] },
];

const INITIAL_DEALS: Deal[] = [
  { id: "1", company: "Acme Corp", value: 5000, stage: "prospecting" },
  { id: "2", company: "TechStart Inc", value: 12000, stage: "qualified" },
  { id: "3", company: "Beta Solutions", value: 8000, stage: "proposal" },
];

function DealCard({
  deal,
  isOverlay,
}: {
  deal: Deal;
  isOverlay?: boolean;
}) {
  const { attributes, listeners, setNodeRef, transform } = useDraggable({
    id: deal.id,
    data: deal,
  });

  const style = transform
    ? {
        transform: CSS.Translate.toString(transform),
      }
    : undefined;

  return (
    <Card
      ref={setNodeRef}
      style={style}
      {...listeners}
      {...attributes}
      className={`cursor-grab active:cursor-grabbing ${
        isOverlay ? "rotate-2 shadow-xl" : ""
      }`}
    >
      <CardContent className="p-3 space-y-2">
        <div className="flex justify-between items-start">
          <h4 className="font-medium text-sm">{deal.company}</h4>
          <Badge variant="secondary" className="text-xs">
            ${deal.value.toLocaleString()}
          </Badge>
        </div>
        {deal.contact && (
          <p className="text-xs text-muted-foreground">{deal.contact}</p>
        )}
        {deal.followUp && (
          <p className="text-xs text-orange-500">
            Follow-up: {new Date(deal.followUp).toLocaleDateString()}
          </p>
        )}
        <div className="flex gap-1 pt-1">
          <QuickLogActivity dealId={deal.id} />
          <QuickSetFollowUp dealId={deal.id} />
        </div>
      </CardContent>
    </Card>
  );
}

function PipelineColumn({
  column,
  deals,
}: {
  column: Column;
  deals: Deal[];
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: column.id,
  });

  const columnValue = deals.reduce((sum, d) => sum + d.value, 0);

  return (
    <div className="flex flex-col w-72 min-w-72">
      <CardHeader className="p-3 pb-2">
        <div className="flex justify-between items-center">
          <h3 className="font-semibold text-sm">{column.title}</h3>
          <Badge variant="outline" className="text-xs">
            {deals.length}
          </Badge>
        </div>
        <p className="text-xs text-muted-foreground">
          ${columnValue.toLocaleString()}
        </p>
      </CardHeader>
      <div
        ref={setNodeRef}
        className={`flex-1 p-2 space-y-2 min-h-[200px] rounded-lg transition-colors ${
          isOver ? "bg-muted/50" : ""
        }`}
      >
        {deals.map((deal) => (
          <DealCard key={deal.id} deal={deal} />
        ))}
      </div>
    </div>
  );
}

export function PipelineBoard() {
  const [deals, setDeals] = useState<Deal[]>(INITIAL_DEALS);
  const [activeId, setActiveId] = useState<string | null>(null);

  const columns = INITIAL_COLUMNS.map((col) => ({
    ...col,
    deals: deals.filter((d) => d.stage === col.id),
  }));

  const handleDragStart = (event: DragStartEvent) => {
    setActiveId(event.active.id as string);
  };

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;

    if (over && active.id !== over.id) {
      const newStage = over.id as string;
      setDeals((deals) =>
        deals.map((d) =>
          d.id === active.id ? { ...d, stage: newStage } : d
        )
      );

      // Trigger n8n webhook
      fetch("/api/webhooks/deal-moved", {
        method: "POST",
        body: JSON.stringify({
          dealId: active.id,
          newStage,
          timestamp: new Date().toISOString(),
        }),
      });
    }

    setActiveId(null);
  };

  const activeDeal = deals.find((d) => d.id === activeId);

  return (
    <DndContext onDragStart={handleDragStart} onDragEnd={handleDragEnd}>
      <div className="flex gap-4 overflow-x-auto pb-4">
        {columns.map((column) => (
          <Card key={column.id} className="flex flex-col bg-muted/30">
            <PipelineColumn column={column} deals={column.deals} />
          </Card>
        ))}
      </div>
      <DragOverlay>
        {activeDeal ? <DealCard deal={activeDeal} isOverlay /> : null}
      </DragOverlay>
    </DndContext>
  );
}
