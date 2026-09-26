/*
Copyright (C) 1996-1997 Id Software, Inc.

This program is free software; you can redistribute it and/or
modify it under the terms of the GNU General Public License
as published by the Free Software Foundation; either version 2
of the License, or (at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.

See the GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program; if not, write to the Free Software
Foundation, Inc., 59 Temple Place - Suite 330, Boston, MA  02111-1307, USA.

*/
// snd_oracle.c -- the quake-rust sound oracle: id's snd_dma.c, snd_mix.c and
// snd_mem.c, headless. This file is everything around them:
//
// - a fake DMA driver in place of snd_win.c / snd_null.c: a 16-bit stereo ring
//   whose play position the script moves (`advance`), and whose SNDDMA_Submit
//   appends every newly painted sample pair to the output;
// - the few engine pieces the mixer calls: cvars, the pak (COM_LoadStackFile),
//   the cache, cl.viewentity, the listener's leaf (Mod_PointInLeaf), and rand
//   (linked as __wrap_rand: the MSVC runtime's rand, which WinQuake.exe used;
//   the port's mixer draws from the same generator);
// - main(), which runs a script of sound calls (oracle/sound.py writes them;
//   `quaketool sndscript` runs the same script through the port):
//
//   snd-oracle <pak> <script> <out.raw> <rate> [<trace.txt>]
//
// Script lines (one call each; `#` starts a comment):
//   cvar NAME VALUE            Cvar_Set (volume, nosound, loadas8bit, ambient_level,
//                              ambient_fade, _snd_mixahead)
//   viewent N                  cl.viewentity
//   listener OX OY OZ FX FY FZ RX RY RZ   S_Update's origin, forward and right
//   leaf A B C D | leaf none   the listener leaf's ambient_sound_level[] (none: NULL)
//   frametime SECONDS          host_frametime
//   start ENT CHAN SAMPLE OX OY OZ VOLBYTE ATTENBYTE
//                              CL_ParseStartSoundPacket's S_StartSound: fvol =
//                              VOLBYTE/255.0, attenuation = ATTENBYTE/64.0
//   static SAMPLE OX OY OZ VOLBYTE ATTENBYTE   CL_ParseStaticSound's S_StaticSound
//   stop ENT CHAN              S_StopSound
//   stopall                    S_StopAllSounds (true)
//   local SAMPLE               S_LocalSound
//   update                     S_Update: respatialize, then S_Update_ mixes ahead
//                              of the DMA position
//   advance N                  the DMA play position moves N sample pairs
//
// The output is raw signed 16-bit little-endian stereo: every sample pair the
// mixer painted, in order (a stretch the mixer skipped because the play
// position overtook it is not there). The trace (optional) prints each channel with a sound
// after every `update`.

#include "quakedef.h"

// ---------------------------------------------------------------- the engine's side

client_state_t	cl;
quakeparms_t	host_parms;
double			host_frametime;
// vec3_origin comes from mathlib.c
int				com_filesize;

static model_t	oracle_world;
static mleaf_t	oracle_leaf;
static qboolean	oracle_leaf_none;

void Sys_Error (char *error, ...)
{
	va_list		argptr;

	va_start (argptr, error);
	fprintf (stderr, "Sys_Error: ");
	vfprintf (stderr, error, argptr);
	fprintf (stderr, "\n");
	va_end (argptr);
	exit (1);
}

void Con_Printf (char *fmt, ...)
{
	va_list		argptr;

	va_start (argptr, fmt);
	vfprintf (stderr, fmt, argptr);
	va_end (argptr);
}

void Con_DPrintf (char *fmt, ...)
{
}

int COM_CheckParm (char *parm)
{
	return 0;
}

void Cmd_AddCommand (char *cmd_name, xcommand_t function)
{
}

int Cmd_Argc (void)
{
	return 0;
}

char *Cmd_Argv (int arg)
{
	return "";
}

// common.c's Q_* helpers (the ones snd_*.c call)
void Q_memset (void *dest, int fill, int count) { memset (dest, fill, count); }
void Q_memcpy (void *dest, void *src, int count) { memcpy (dest, src, count); }
void Q_strcpy (char *dest, char *src) { strcpy (dest, src); }
void Q_strcat (char *dest, char *src) { strcat (dest, src); }
int Q_strlen (char *str) { return strlen (str); }
int Q_strcmp (char *s1, char *s2) { return strcmp (s1, s2); }
int Q_strncmp (char *s1, char *s2, int count) { return strncmp (s1, s2, count); }
char *Q_strrchr (char *s, char c) { return strrchr (s, c); }

// common.c's Q_atof, as id wrote it: the cvar values go through this
float Q_atof (char *str)
{
	double			val;
	int             sign;
	int             c;
	int             decimal, total;

	if (*str == '-')
	{
		sign = -1;
		str++;
	}
	else
		sign = 1;

	val = 0;
	decimal = -1;
	total = 0;
	while (1)
	{
		c = *str++;
		if (c == '.')
		{
			decimal = total;
			continue;
		}
		if (c <'0' || c > '9')
			break;
		val = val*10 + c - '0';
		total++;
	}

	if (decimal == -1)
		return val*sign;
	while (total > decimal)
	{
		val /= 10;
		total--;
	}

	return val*sign;
}

// common.c's byte order (i386 is little-endian)
static short ShortNoSwap (short l) { return l; }
static int LongNoSwap (int l) { return l; }
static float FloatNoSwap (float l) { return l; }
short	(*LittleShort) (short l) = ShortNoSwap;
int		(*LittleLong) (int l) = LongNoSwap;
float	(*LittleFloat) (float l) = FloatNoSwap;

// cvar.c, the part the mixer uses
cvar_t	*cvar_vars;

void Cvar_RegisterVariable (cvar_t *variable)
{
	char	*oldstr;

	oldstr = variable->string;
	variable->string = malloc (strlen (oldstr) + 1);
	strcpy (variable->string, oldstr);
	variable->value = Q_atof (variable->string);
	variable->next = cvar_vars;
	cvar_vars = variable;
}

void Cvar_Set (char *var_name, char *value)
{
	cvar_t	*var;

	for (var = cvar_vars ; var ; var = var->next)
		if (!strcmp (var_name, var->name))
			break;
	if (!var)
		Sys_Error ("Cvar_Set: variable %s not found", var_name);
	var->string = malloc (strlen (value) + 1);
	strcpy (var->string, value);
	var->value = Q_atof (var->string);
}

// zone.c: the hunk and the cache, as plain allocations that are never flushed
void *Hunk_AllocName (int size, char *name)
{
	return calloc (1, size);
}

void *Cache_Check (cache_user_t *c)
{
	return c->data;
}

void *Cache_Alloc (cache_user_t *c, int size, char *name)
{
	c->data = calloc (1, size);
	return c->data;
}

// model.c: the listener's leaf is whatever the script said
mleaf_t *Mod_PointInLeaf (vec3_t p, model_t *model)
{
	return oracle_leaf_none ? NULL : &oracle_leaf;
}

// the MSVC runtime's rand() (srand(1) at start), linked over libc's
static unsigned int	oracle_holdrand = 1;

int __wrap_rand (void)
{
	oracle_holdrand = oracle_holdrand * 214013 + 2531011;
	return (oracle_holdrand >> 16) & 0x7fff;
}

// ---------------------------------------------------------------- the pak

static byte		*pak_bytes;
static int		pak_numfiles;
static byte		*pak_dir;

static void Pak_Open (char *path)
{
	FILE	*f;
	long	len;

	f = fopen (path, "rb");
	if (!f)
		Sys_Error ("cannot open %s", path);
	fseek (f, 0, SEEK_END);
	len = ftell (f);
	fseek (f, 0, SEEK_SET);
	pak_bytes = malloc (len);
	if (fread (pak_bytes, 1, len, f) != (size_t)len)
		Sys_Error ("cannot read %s", path);
	fclose (f);
	if (memcmp (pak_bytes, "PACK", 4))
		Sys_Error ("%s is not a pak", path);
	pak_dir = pak_bytes + LittleLong (*(int *)(pak_bytes + 4));
	pak_numfiles = LittleLong (*(int *)(pak_bytes + 8)) / 64;
}

// common.c's COM_LoadStackFile, from the one pak (the mixer frees nothing)
byte *COM_LoadStackFile (char *path, void *buffer, int bufsize)
{
	int		i, pos, len;
	byte	*data;

	for (i = 0 ; i < pak_numfiles ; i++)
	{
		if (strcmp ((char *)pak_dir + i*64, path))
			continue;
		pos = LittleLong (*(int *)(pak_dir + i*64 + 56));
		len = LittleLong (*(int *)(pak_dir + i*64 + 60));
		data = malloc (len + 1);
		memcpy (data, pak_bytes + pos, len);
		data[len] = 0;
		com_filesize = len;
		return data;
	}
	return NULL;
}

// ---------------------------------------------------------------- the fake DMA

#define RING_SAMPLES	65536		// mono samples: 32768 stereo pairs

static short	ring[RING_SAMPLES];
static int		oracle_rate;
static int		dma_pairs;			// the play position, in sample pairs (never wraps)
static int		submitted;			// paintedtime at the last SNDDMA_Submit

static short	*out_pcm;
static int		out_len, out_cap;	// in shorts

extern int		paintedtime;
extern int		soundtime;

qboolean SNDDMA_Init (void)
{
	shm = &sn;
	shm->splitbuffer = 0;
	shm->samplebits = 16;
	shm->speed = oracle_rate;
	shm->channels = 2;
	shm->samples = RING_SAMPLES;
	shm->samplepos = 0;
	shm->soundalive = true;
	shm->gamealive = true;
	shm->submission_chunk = 1;
	shm->buffer = (unsigned char *)ring;
	return 1;
}

int SNDDMA_GetDMAPos (void)
{
	shm->samplepos = (dma_pairs * shm->channels) & (shm->samples - 1);
	return shm->samplepos;
}

// every pair painted since the last submit, in order. If the play position
// overtook the mixer, S_Update_ skipped paintedtime up to soundtime first: the
// pairs skipped were never painted and are not output.
void SNDDMA_Submit (void)
{
	int		t, mask;

	mask = (shm->samples >> 1) - 1;
	if (submitted < soundtime)
		submitted = soundtime;
	if (paintedtime - submitted > (shm->samples >> 1))
		Sys_Error ("SNDDMA_Submit: %d pairs painted at once, more than the ring", paintedtime - submitted);
	for (t = submitted ; t < paintedtime ; t++)
	{
		if (out_len + 2 > out_cap)
		{
			out_cap = out_cap ? out_cap * 2 : 1 << 16;
			out_pcm = realloc (out_pcm, out_cap * sizeof(short));
		}
		out_pcm[out_len++] = ring[(t & mask) * 2];
		out_pcm[out_len++] = ring[(t & mask) * 2 + 1];
	}
	submitted = paintedtime;
}

void SNDDMA_Shutdown (void)
{
}

// ---------------------------------------------------------------- the script

static vec3_t	l_origin, l_forward, l_right, l_up;

static void Trace (FILE *f)
{
	int			i;
	channel_t	*ch;

	fprintf (f, "update paintedtime %d total_channels %d\n", paintedtime, total_channels);
	for (i = 0, ch = channels ; i < total_channels ; i++, ch++)
	{
		if (!ch->sfx)
			continue;
		fprintf (f, "  ch %d %s left %d right %d master %d pos %d end %d ent %d chan %d\n",
			i, ch->sfx->name, ch->leftvol, ch->rightvol, ch->master_vol,
			ch->pos, ch->end, ch->entnum, ch->entchannel);
	}
}

static void Run (char *path, FILE *trace)
{
	FILE	*f;
	char	line[1024], cmd[64], a[256], b[256];
	int		ent, chan, volb, attb, n, lineno;
	int		lv[4];
	float	o[3], fw[3], rt[3];
	sfx_t	*sfx;

	f = fopen (path, "r");
	if (!f)
		Sys_Error ("cannot open %s", path);
	lineno = 0;
	while (fgets (line, sizeof(line), f))
	{
		lineno++;
		if (strchr (line, '#'))
			*strchr (line, '#') = 0;
		if (sscanf (line, "%63s", cmd) != 1)
			continue;

		if (!strcmp (cmd, "cvar") && sscanf (line, "%*s %255s %255s", a, b) == 2)
			Cvar_Set (a, b);
		else if (!strcmp (cmd, "viewent") && sscanf (line, "%*s %d", &n) == 1)
			cl.viewentity = n;
		else if (!strcmp (cmd, "listener") && sscanf (line, "%*s %f %f %f %f %f %f %f %f %f",
			&o[0], &o[1], &o[2], &fw[0], &fw[1], &fw[2], &rt[0], &rt[1], &rt[2]) == 9)
		{
			VectorCopy (o, l_origin);
			VectorCopy (fw, l_forward);
			VectorCopy (rt, l_right);
		}
		else if (!strcmp (cmd, "leaf") && sscanf (line, "%*s %255s", a) == 1 && !strcmp (a, "none"))
			oracle_leaf_none = true;
		else if (!strcmp (cmd, "leaf") && sscanf (line, "%*s %d %d %d %d", &lv[0], &lv[1], &lv[2], &lv[3]) == 4)
		{
			oracle_leaf_none = false;
			for (n = 0 ; n < 4 ; n++)
				oracle_leaf.ambient_sound_level[n] = lv[n];
		}
		else if (!strcmp (cmd, "frametime") && sscanf (line, "%*s %lf", &host_frametime) == 1)
			;
		else if (!strcmp (cmd, "start") && sscanf (line, "%*s %d %d %255s %f %f %f %d %d",
			&ent, &chan, a, &o[0], &o[1], &o[2], &volb, &attb) == 8)
		{
			sfx = S_PrecacheSound (a);
			S_StartSound (ent, chan, sfx, o, volb/255.0, attb/64.0);
		}
		else if (!strcmp (cmd, "static") && sscanf (line, "%*s %255s %f %f %f %d %d",
			a, &o[0], &o[1], &o[2], &volb, &attb) == 6)
		{
			sfx = S_PrecacheSound (a);
			S_StaticSound (sfx, o, volb, attb);
		}
		else if (!strcmp (cmd, "stop") && sscanf (line, "%*s %d %d", &ent, &chan) == 2)
			S_StopSound (ent, chan);
		else if (!strcmp (cmd, "stopall"))
			S_StopAllSounds (true);
		else if (!strcmp (cmd, "local") && sscanf (line, "%*s %255s", a) == 1)
			S_LocalSound (a);
		else if (!strcmp (cmd, "update"))
		{
			S_Update (l_origin, l_forward, l_right, l_up);
			if (trace)
				Trace (trace);
		}
		else if (!strcmp (cmd, "advance") && sscanf (line, "%*s %d", &n) == 1)
			dma_pairs += n;
		else
			Sys_Error ("%s:%d: cannot parse: %s", path, lineno, line);
	}
	fclose (f);
}

int main (int argc, char **argv)
{
	FILE	*out, *trace;

	if (argc < 5)
	{
		fprintf (stderr, "usage: snd-oracle <pak> <script> <out.raw> <rate> [<trace.txt>]\n");
		return 2;
	}
	Pak_Open (argv[1]);
	oracle_rate = atoi (argv[4]);
	host_parms.memsize = 16*1024*1024;		// >= 8 MB: loadas8bit stays 0
	cl.worldmodel = &oracle_world;
	trace = NULL;
	if (argc > 5)
	{
		trace = fopen (argv[5], "w");
		if (!trace)
			Sys_Error ("cannot write %s", argv[5]);
	}

	S_Init ();
	Run (argv[2], trace);

	out = fopen (argv[3], "wb");
	if (!out)
		Sys_Error ("cannot write %s", argv[3]);
	fwrite (out_pcm, sizeof(short), out_len, out);
	fclose (out);
	if (trace)
		fclose (trace);
	return 0;
}
