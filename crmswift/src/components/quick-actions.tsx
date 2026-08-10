"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Plus, Phone, Mail, Clock, CheckCircle } from "lucide-react";

// Quick Add Deal Modal
export function QuickAddDeal() {
  const [open, setOpen] = useState(false);
  const [company, setCompany] = useState("");
  const [value, setValue] = useState("");

  const handleSubmit = () => {
    // Send to n8n webhook
    fetch("/api/webhooks/deal-created", {
      method: "POST",
      body: JSON.stringify({ company, value, stage: "prospecting" }),
    });
    setOpen(false);
    setCompany("");
    setValue("");
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button size="sm" className="gap-2">
          <Plus className="w-4 h-4" />
          Add Deal
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Quick Add Deal</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 pt-4">
          <Input
            placeholder="Company name"
            value={company}
            onChange={(e) => setCompany(e.target.value)}
            autoFocus
          />
          <Input
            placeholder="Deal value ($)"
            type="number"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
          <Button onClick={handleSubmit} className="w-full">
            Add Deal
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

// Quick Log Activity Modal
export function QuickLogActivity({ dealId }: { dealId: string }) {
  const [open, setOpen] = useState(false);
  const [note, setNote] = useState("");
  const [type, setType] = useState<"call" | "email" | "meeting">("call");

  const handleSubmit = () => {
    fetch("/api/webhooks/activity-logged", {
      method: "POST",
      body: JSON.stringify({ dealId, type, note }),
    });
    setOpen(false);
    setNote("");
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="sm">
          Log
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Log Activity</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 pt-4">
          <div className="flex gap-2">
            <Button
              variant={type === "call" ? "default" : "outline"}
              size="sm"
              onClick={() => setType("call")}
              className="flex-1 gap-2"
            >
              <Phone className="w-4 h-4" />
              Call
            </Button>
            <Button
              variant={type === "email" ? "default" : "outline"}
              size="sm"
              onClick={() => setType("email")}
              className="flex-1 gap-2"
            >
              <Mail className="w-4 h-4" />
              Email
            </Button>
          </div>
          <Textarea
            placeholder="What happened?"
            value={note}
            onChange={(e) => setNote(e.target.value)}
            autoFocus
          />
          <Button onClick={handleSubmit} className="w-full">
            Log {type}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

// Quick Set Follow-up Modal
export function QuickSetFollowUp({ dealId }: { dealId: string }) {
  const [open, setOpen] = useState(false);
  const [date, setDate] = useState("");

  const handleSubmit = () => {
    fetch("/api/webhooks/follow-up-set", {
      method: "POST",
      body: JSON.stringify({ dealId, date }),
    });
    setOpen(false);
    setDate("");
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="sm">
          <Clock className="w-4 h-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Set Follow-up</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 pt-4">
          <Input
            type="datetime-local"
            value={date}
            onChange={(e) => setDate(e.target.value)}
            autoFocus
          />
          <Button onClick={handleSubmit} className="w-full">
            Set Reminder
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

// Quick Move Stage Modal
export function QuickMoveStage({
  dealId,
  currentStage,
  stages,
}: {
  dealId: string;
  currentStage: string;
  stages: string[];
}) {
  const [open, setOpen] = useState(false);

  const handleMove = (stage: string) => {
    fetch("/api/webhooks/deal-moved", {
      method: "POST",
      body: JSON.stringify({ dealId, stage }),
    });
    setOpen(false);
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="sm">
          Move
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Move to Stage</DialogTitle>
        </DialogHeader>
        <div className="space-y-2 pt-4">
          {stages.map((stage) => (
            <Button
              key={stage}
              variant={stage === currentStage ? "default" : "outline"}
              className="w-full justify-start"
              onClick={() => handleMove(stage)}
            >
              {stage === currentStage && (
                <CheckCircle className="w-4 h-4 mr-2" />
              )}
              {stage}
            </Button>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
