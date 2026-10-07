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
// sys_oracle.c -- the quake-rust oracle's system driver: id's sys_null.c file IO,
// plus a deterministic clock (every Host_Frame advances exactly -oracle_dt seconds,
// default 0.1, and Sys_FloatTime returns that virtual clock) or, with
// -oracle_realtime, the wall clock (for timedemo).
//
// -oracle_loadtime T: a level load (SV_SpawnServer) takes T seconds of the
// virtual clock, as loads took seconds of real time on id's machines: the host
// frame after one is handed T more, so Host_FilterTime clamps it to 0.1 s as it
// did there. Default 0, an instant load.

#include "quakedef.h"
#include "errno.h"
#include <time.h>

qboolean		isDedicated;

static qboolean	oracle_realtime;
static double	oracle_clock;		// virtual seconds since start (deterministic mode)
static qboolean	oracle_loaded;		// SV_SpawnServer ran in this host frame

void __real_SV_SpawnServer (char *server);

void __wrap_SV_SpawnServer (char *server)
{
	__real_SV_SpawnServer (server);
	oracle_loaded = true;
}

/*
===============================================================================

FILE IO (sys_null.c)

===============================================================================
*/

#define MAX_HANDLES             10
FILE    *sys_handles[MAX_HANDLES];

int             findhandle (void)
{
	int             i;

	for (i=1 ; i<MAX_HANDLES ; i++)
		if (!sys_handles[i])
			return i;
	Sys_Error ("out of handles");
	return -1;
}

int filelength (FILE *f)
{
	int             pos;
	int             end;

	pos = ftell (f);
	fseek (f, 0, SEEK_END);
	end = ftell (f);
	fseek (f, pos, SEEK_SET);

	return end;
}

int Sys_FileOpenRead (char *path, int *hndl)
{
	FILE    *f;
	int             i;

	i = findhandle ();

	f = fopen(path, "rb");
	if (!f)
	{
		*hndl = -1;
		return -1;
	}
	sys_handles[i] = f;
	*hndl = i;

	return filelength(f);
}

int Sys_FileOpenWrite (char *path)
{
	FILE    *f;
	int             i;

	i = findhandle ();

	f = fopen(path, "wb");
	if (!f)
		Sys_Error ("Error opening %s: %s", path,strerror(errno));
	sys_handles[i] = f;

	return i;
}

void Sys_FileClose (int handle)
{
	fclose (sys_handles[handle]);
	sys_handles[handle] = NULL;
}

void Sys_FileSeek (int handle, int position)
{
	fseek (sys_handles[handle], position, SEEK_SET);
}

int Sys_FileRead (int handle, void *dest, int count)
{
	return fread (dest, 1, count, sys_handles[handle]);
}

int Sys_FileWrite (int handle, void *data, int count)
{
	return fwrite (data, 1, count, sys_handles[handle]);
}

int     Sys_FileTime (char *path)
{
	FILE    *f;

	f = fopen(path, "rb");
	if (f)
	{
		fclose(f);
		return 1;
	}

	return -1;
}

void Sys_mkdir (char *path)
{
}


/*
===============================================================================

SYSTEM IO

===============================================================================
*/

void Sys_MakeCodeWriteable (unsigned long startaddr, unsigned long length)
{
}

void Sys_DebugLog(char *file, char *fmt, ...)
{
}

void Sys_Error (char *error, ...)
{
	va_list         argptr;

	printf ("Sys_Error: ");
	va_start (argptr,error);
	vprintf (error,argptr);
	va_end (argptr);
	printf ("\n");
	fflush (stdout);

	exit (1);
}

void Sys_Printf (char *fmt, ...)
{
	va_list         argptr;

	if (COM_CheckParm ("-oracle_quiet"))
		return;
	va_start (argptr,fmt);
	vprintf (fmt,argptr);
	va_end (argptr);
}

// Quits WITHOUT Host_Shutdown: the oracle never writes config.cfg, so one run
// cannot leak cvars (viewsize, r_drawentities, ...) into the next.
void Sys_Quit (void)
{
	fflush (stdout);
	exit (0);
}

static double Sys_WallTime (void)
{
	struct timespec	ts;

	clock_gettime (CLOCK_MONOTONIC, &ts);
	return ts.tv_sec + ts.tv_nsec * 1e-9;
}

double Sys_FloatTime (void)
{
	static double	base;

	if (!oracle_realtime)
		return oracle_clock;
	if (!base)
		base = Sys_WallTime ();
	return Sys_WallTime () - base;
}

char *Sys_ConsoleInput (void)
{
	return NULL;
}

void Sys_Sleep (void)
{
}

void Sys_SendKeyEvents (void)
{
}

// -oracle_fpcw: the FPU state id's x86 builds rendered in. R_RenderView_ calls
// Sys_LowFPPrecision once the frame is set up and Sys_HighFPPrecision at its
// end; sys_wina.s / sys_dosa.s load single_cw there ("make FDIV fast": the x87
// precision control at 24 bits, and chop rounding) and full_cw (64 bits,
// round to nearest) back (Sys_SetFPCW). Without the flag, null drivers' no-ops,
// as before. The x87 build only: SSE arithmetic does not read the x87 word.
static qboolean	oracle_fpcw;

static void Oracle_LoadFPCW (unsigned short rcpc)
{
	unsigned short	cw;

	__asm__ volatile ("fnstcw %0" : "=m" (cw));
	cw = (cw & 0xF0FF) | (rcpc << 8);
	__asm__ volatile ("fldcw %0" : : "m" (cw));
}

void Sys_HighFPPrecision (void)
{
	if (oracle_fpcw)
		Oracle_LoadFPCW (0x03);	// round mode, 64-bit precision
}

void Sys_LowFPPrecision (void)
{
	if (oracle_fpcw)
		Oracle_LoadFPCW (0x0C);	// chop mode, single precision
}

void Sys_SetFPCW (void)
{
}

//=============================================================================

int main (int argc, char **argv)
{
	static quakeparms_t    parms;
	double		dt, loadtime, pending, oldtime, newtime;
	int			i;

	parms.memsize = 32*1024*1024;
	COM_InitArgv (argc, argv);
	oracle_fpcw = COM_CheckParm ("-oracle_fpcw") != 0;
	parms.argc = com_argc;
	parms.argv = com_argv;

	i = COM_CheckParm ("-mem");
	if (i && i < com_argc-1)
		parms.memsize = (int) (Q_atof (com_argv[i+1]) * 1024 * 1024);
	parms.membase = malloc (parms.memsize);
	if (!parms.membase)
		Sys_Error ("Can't allocate %d bytes\n", parms.memsize);
	parms.basedir = ".";

	oracle_realtime = COM_CheckParm ("-oracle_realtime") != 0;
	dt = 0.1;
	i = COM_CheckParm ("-oracle_dt");
	if (i && i < com_argc-1)
		dt = Q_atof (com_argv[i+1]);
	loadtime = 0;
	i = COM_CheckParm ("-oracle_loadtime");
	if (i && i < com_argc-1)
		loadtime = Q_atof (com_argv[i+1]);
	pending = 0;

	Host_Init (&parms);

	oldtime = Sys_FloatTime ();
	while (1)
	{
		if (oracle_realtime)
		{
			newtime = Sys_FloatTime ();
			Host_Frame (newtime - oldtime);
			oldtime = newtime;
		}
		else
		{
			oracle_clock += dt + pending;
			Host_Frame (dt + pending);
			pending = oracle_loaded ? loadtime : 0;
			oracle_loaded = false;
		}
	}
	return 0;
}
