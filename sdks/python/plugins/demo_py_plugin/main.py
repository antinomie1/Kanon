"""Demonstration Python plugin for Kanon microkernel."""

import asyncio
import random
from typing import Any, Dict, Literal, Optional

from kanon_sdk import (
    CommandEvent,
    HttpRequest,
    MessageEvent,
    MessageSegment,
    Plugin,
    PluginContext,
    command,
    http_route,
    on_llm_request,
    tool,
    trigger,
)
from kanon_sdk.proto import pb


class DemoPythonPlugin(Plugin):
    """Demonstration plugin implemented in Python."""

    id = "org.kanon.plugin.demo_py"
    name = "Demo Python Plugin"
    version = "0.1.0"
    author = "Kanon Dev"
    description = "Demonstration plugin written in Python"
    priority = 100

    async def on_load(self, ctx: PluginContext) -> None:
        print(f"Demo Python Plugin initialized with data directory: {ctx.data_dir}", flush=True)

    async def on_pre_filter(
        self,
        req: pb.PipelineEventRequest,
    ) -> Optional[pb.PreFilterResult]:
        if "[block]" in req.raw_text:
            return pb.PreFilterResult(
                action=pb.PreFilterResult.Action.BLOCK,
                modified_text="",
                reply_messages=[
                    MessageSegment.text("Message blocked by Demo Python Plugin pre-filter")
                ],
            )
        return None

    @command(
        name="pycalc",
        description="Python-based calculation command",
        usage="/pycalc <expr>",
        priority=100,
        aliases=("pc",),
    )
    async def handle_pycalc(self, event: CommandEvent) -> str:
        # Returning text is the shortest way to answer.
        return f"Python calculation result for [{' '.join(event.args)}]: 42 (fast-path)"

    @command(name="guess", description="Guess a number between 1 and 10", usage="/guess")
    async def handle_guess(self, event: CommandEvent) -> None:
        # A multi-turn conversation: each wait_next sends the replies so far and resumes with the
        # same sender's next message in this channel.
        secret = random.randint(1, 10)
        await event.reply("I picked a number between 1 and 10. Your guess?")
        for _ in range(3):
            try:
                answer = await event.wait_next(timeout=60)
            except asyncio.TimeoutError:
                await event.reply(f"Time is up — it was {secret}.")
                return
            if not answer.text.strip().isdigit():
                await answer.reply("Please answer with a number.")
                continue
            guess = int(answer.text.strip())
            if guess == secret:
                await answer.reply("Correct!")
                return
            await answer.reply("Higher." if guess < secret else "Lower.")
        await event.reply(f"Out of guesses — it was {secret}.")

    @trigger(r"^(?:hi|hello) py$", description="Greets back")
    async def greet(self, event: MessageEvent) -> list:
        return [MessageSegment.quote(event.event_id), MessageSegment.text("Hello from Python!")]

    @tool(
        name="py_calc",
        description="Python mathematical calculation tool",
        parameters={
            "type": "object",
            "properties": {
                "expr": {"type": "string", "description": "Expression to evaluate"},
            },
        },
    )
    async def handle_py_calc(self, params: Dict[str, Any]) -> Dict[str, Any]:
        return {
            "result": 42.0,
            "summary": "Calculated via Python plugin tool",
        }

    # --- Command groups backed by the node's KV store -------------------------------------------
    # "/note add <text>" and "/note list" form one group; "/note" alone lists its subcommands.

    @command("note add", description="Save a note for this chat", usage="/note add <text>")
    async def note_add(self, event: CommandEvent) -> str:
        if not event.raw_args:
            return "Usage: /note add <text>"
        key = f"notes:{event.channel_id}"
        notes = await self.kv.get(key, [])
        notes.append(event.raw_args)
        await self.kv.set(key, notes)
        return f"Saved note #{len(notes)}."

    @command("note list", description="Show this chat's notes")
    async def note_list(self, event: CommandEvent) -> str:
        notes = await self.kv.get(f"notes:{event.channel_id}", [])
        return "\n".join(f"{i}. {note}" for i, note in enumerate(notes, 1)) or "No notes yet."

    # --- A tool described by its signature -------------------------------------------------------

    @tool
    async def dice(self, sides: int = 6, count: int = 1, mode: Literal["sum", "each"] = "sum") -> Any:
        """Rolls dice for the user.

        Args:
            sides: Faces on each die.
            count: How many dice to roll, at most 20.
            mode: Return the total, or every roll.
        """
        if not 1 <= count <= 20 or sides < 2:
            raise ValueError("count must be 1-20 and sides at least 2")
        rolls = [random.randint(1, sides) for _ in range(count)]
        return sum(rolls) if mode == "sum" else rolls

    # --- Rewriting the system prompt for one chat ------------------------------------------------

    @on_llm_request
    async def chat_rules(self, event: MessageEvent, system_prompt: str) -> Optional[str]:
        rules = await self.kv.get(f"rules:{event.channel_id}")
        if not rules:
            return None  # keep the prompt unchanged
        return f"{system_prompt}\n\nRules for this chat:\n{rules}"

    @command(
        "rules",
        description="Set extra instructions for the assistant in this chat",
        usage="/rules [text]",
        access="admins_in_groups",
    )
    async def set_rules(self, event: CommandEvent) -> str:
        if event.raw_args:
            await self.kv.set(f"rules:{event.channel_id}", event.raw_args)
            return "Rules saved; the assistant follows them from the next message."
        await self.kv.delete(f"rules:{event.channel_id}")
        return "Rules cleared."

    # --- An HTTP route under /api/v1/plugins/org.kanon.plugin.demo_py/http/ -----------------------

    @http_route("/notes")
    async def notes_api(self, request: HttpRequest) -> Dict[str, Any]:
        channel = request.arg("channel", "")
        return {"channel": channel, "notes": await self.kv.get(f"notes:{channel}", [])}
